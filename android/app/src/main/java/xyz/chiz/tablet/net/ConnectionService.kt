package xyz.chiz.tablet.net

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import androidx.datastore.preferences.core.edit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import org.json.JSONObject
import xyz.chiz.tablet.proto.PanicGesture
import xyz.chiz.tablet.proto.StripProfile
import xyz.chiz.tablet.proto.parseStripProfile
import xyz.chiz.tablet.proto.toHex
import xyz.chiz.tablet.ui.MainActivity
import xyz.chiz.tablet.ui.PenAreaView
import xyz.chiz.tablet.ui.SettingsKeys
import xyz.chiz.tablet.ui.settingsStore
import java.net.InetAddress
import java.util.Base64

/** Connection status for the UI (derived from [LinkState]). */
data class ConnState(
    val status: String = "disconnected", // disconnected|connecting|reconnecting|connected|error
    val pcName: String = "",
    val message: String = "",
    val attemptsLeft: Int = 5,
    /** Host whose certificate mismatched: auto-connect must skip it. */
    val mismatchHost: String = "",
)

/** How this tablet will prove itself; built by the UI before connecting. */
sealed interface PairInfo {
    class Paired(val secretB64: String, val pinnedFp: ByteArray) : PairInfo
    class Qr(val hosts: List<String>, val port: Int, val tokenB64: String, val pinnedFp: ByteArray) : PairInfo
    class Pin(val pin: String) : PairInfo
}

/**
 * Foreground service (type connectedDevice) that keeps the session alive
 * across rotation and screen changes (spec 6). All networking lives in
 * [Connector]; this class adapts it to Android: pen sender, Wi-Fi/wake
 * locks, audio feedback, pairing storage and the notification.
 */
class ConnectionService : Service(), ConnectorHost {
    inner class LocalBinder(val service: ConnectionService) : Binder()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val main = Handler(Looper.getMainLooper())
    private val connector by lazy { Connector(scope, this) }
    private val pairingStore by lazy { PairingStore(this) }
    val audio: AudioEngine by lazy { AudioEngine(this) }

    private val _state = MutableStateFlow(ConnState())
    val state: StateFlow<ConnState> = _state
    private val _profile = MutableStateFlow<StripProfile?>(null)
    val profile: StateFlow<StripProfile?> = _profile
    private val _toggleStates = MutableStateFlow<Map<String, Boolean>>(emptyMap())
    val toggleStates: StateFlow<Map<String, Boolean>> = _toggleStates
    private val _pairingsVersion = MutableStateFlow(0)
    /** Incremented whenever the pairing store changes; the UI reloads on it. */
    val pairingsVersion: StateFlow<Int> = _pairingsVersion

    val rttMs: Double get() = connector.rttMs.toDouble()

    @Volatile private var sender: PenSender? = null
    private var penView: PenAreaView? = null // main thread only
    @Volatile private var surface = 2100 to 1600
    @Volatile private var panicUntil = 0L
    @Volatile private var note = "" // shown while idle (e.g. "panic disconnect")
    @Volatile private var lastPcName = ""
    @Volatile private var muted = false
    private var muteJob: Job? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    @Volatile override var deviceId: String = ""
        private set
    override val deviceName: String = Build.MODEL ?: "Android tablet"

    override fun onBind(intent: Intent?): IBinder = LocalBinder(this)

    override fun onCreate() {
        super.onCreate()
        createChannel()
        audio.start {}
        scope.launch { connector.state.collect { render(it) } }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_DISCONNECT) {
            disconnect("bye")
            stopSelf()
            return START_NOT_STICKY
        }
        startForeground(
            1,
            notification("Not connected"),
            android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
        )
        return START_STICKY
    }

    override fun onDestroy() {
        connector.stop(bye = true)
        sender?.stop()
        releaseLocks()
        audio.stop()
        scope.cancel()
        super.onDestroy()
    }

    // ------------------------------------------------------------ UI API

    fun isPanicCooldown(): Boolean = SystemClock.uptimeMillis() < panicUntil

    fun connect(host: String, port: Int, deviceIdIn: String, info: PairInfo) =
        connectHosts(listOf(host), port, deviceIdIn, info)

    /** Connect (or pair) trying [hosts] in order. Replaces any running attempt. */
    fun connectHosts(hosts: List<String>, port: Int, deviceIdIn: String, info: PairInfo) {
        if (isPanicCooldown()) {
            _state.value = ConnState("error", "", "Panic cooldown: wait a few seconds.")
            return
        }
        val addrs = hosts.map { it.trim() }.filter { it.isNotEmpty() }.distinct()
        if (addrs.isEmpty()) {
            _state.value = ConnState("error", "", "No address to connect to.")
            return
        }
        deviceId = deviceIdIn
        note = ""
        connector.start(Target(addrs, port, info.toAuth()))
    }

    /** User-initiated disconnect: the PC is told (`bye`). */
    fun disconnect(reason: String) {
        note = if (reason == "bye") "" else reason
        connector.stop(bye = true)
    }

    /** Tablet panic gesture: stop the pen stream first, then bye, then a cooldown. */
    fun panicDisconnect() {
        sender?.stop()
        panicUntil = SystemClock.uptimeMillis() + PanicGesture.RECONNECT_COOLDOWN_MS
        note = "panic disconnect"
        connector.stop(bye = true)
    }

    /** After the user forgets a PC: drop a stale error banner. */
    fun clearError() {
        if (_state.value.status == "error") {
            note = ""
            connector.stop(bye = false)
        }
    }

    fun sendButton(id: String, phase: String) {
        connector.send(JSONObject().put("t", "button").put("id", id).put("phase", phase).toString())
    }

    fun sendSurface(w: Int, h: Int) {
        if (w <= 0 || h <= 0) return
        surface = w to h
        connector.send(JSONObject().put("t", "surface").put("w", w).put("h", h).toString())
    }

    /** The drawing screen hands over its pen view (main thread). */
    fun registerPenView(v: PenAreaView?) {
        penView = v
        v?.sender = sender
    }

    // ------------------------------------------------------------ ConnectorHost

    override fun penArea(): Pair<Int, Int> = surface

    override suspend fun savePairing(host: String, port: Int, pcName: String, fingerprint: ByteArray, secret: ByteArray) {
        val fpHex = fingerprint.toHex()
        val rec = PairedPc(
            pcId = "$host:$port",
            name = pcName.ifEmpty { host },
            fingerprintHex = fpHex,
            secretB64 = Base64.getEncoder().encodeToString(secret),
            hosts = listOf(host),
        )
        try {
            // A re-pair REPLACES any record for the same PC (id, address or certificate).
            val others = pairingStore.load().filter {
                it.pcId != rec.pcId && host !in it.hosts && it.fingerprintHex != fpHex
            }
            pairingStore.save(listOf(rec) + others)
            _pairingsVersion.value += 1
        } catch (e: Exception) {
            note = "Paired, but saving the pairing failed: ${e.message}"
        }
    }

    override suspend fun repairPin(stale: ByteArray, actual: ByteArray) {
        val staleHex = stale.toHex()
        try {
            val fixed = pairingStore.load().map {
                if (it.fingerprintHex == staleHex) it.copy(fingerprintHex = actual.toHex()) else it
            }
            pairingStore.save(fixed)
            _pairingsVersion.value += 1
        } catch (e: Exception) {
            // Harmless: the legacy form keeps working and is repaired next time.
        }
    }

    override fun onConnected(info: SessionInfo) {
        lastPcName = info.pcName
        val s = PenSender(
            InetAddress.getByName(info.host), info.penPort, info.sessionId, info.penKey,
            // Rare (2^32 datagrams): start a fresh session before seq wraps (spec 5).
            onSeqRollover = { connector.send("""{"t":"bye"}""") },
        )
        sender = s
        main.post {
            penView?.sender = s
            penView?.newSession()
        }
        holdLocks()
        scope.launch {
            applicationContext.settingsStore.edit {
                it[SettingsKeys.LAST_HOST] = info.host
                it[SettingsKeys.LAST_PORT] = info.port
            }
        }
        audio.earcon("connect")
        audio.speak("Connected to ${info.pcName}")
        startMuteWatch()
    }

    override fun onSessionEnded(reason: String) {
        sender?.stop()
        sender = null
        main.post { penView?.sender = null }
        releaseLocks()
        muteJob?.cancel()
        _toggleStates.value = emptyMap()
        audio.earcon("disconnect")
        audio.speak("Disconnected")
    }

    override fun onMessage(msg: JSONObject) {
        when (msg.optString("t")) {
            "profile" -> onProfile(msg)
            "button_state" -> {
                val id = msg.optString("id")
                val on = msg.optBoolean("on")
                _toggleStates.value = _toggleStates.value + (id to on)
                val label = _profile.value?.button(id)?.let { it.speak.ifEmpty { it.label } } ?: id
                audio.speak("$label ${if (on) "on" else "off"}")
                audio.earcon(if (on) "on" else "off")
            }
            "speak" -> audio.speak(msg.optString("text"))
        }
    }

    private fun onProfile(o: JSONObject) {
        val p = parseStripProfile(o.getJSONObject("profile"))
        _profile.value = p
        _toggleStates.value = emptyMap()
        val texts = p.buttons.map { it.speak.ifEmpty { it.label } } +
            p.buttons.filter { it.kind == "toggle" }.flatMap {
                val s = it.speak.ifEmpty { it.label }
                listOf("$s on", "$s off")
            }
        audio.precache(texts)
        if (o.optString("reason") in setOf("app", "manual")) audio.speak(p.name)
    }

    // ------------------------------------------------------------ state -> UI

    private fun render(s: LinkState) {
        _state.value = when (s) {
            LinkState.Idle -> ConnState("disconnected", "", note)
            is LinkState.Connecting -> ConnState("connecting", s.host, if (s.attempt > 0) "Trying again…" else "")
            is LinkState.Pairing -> ConnState("connecting", s.host, "Pairing…")
            is LinkState.Connected -> ConnState(
                "connected", s.info.pcName,
                if (muted) "Volume is muted: button feedback will not be heard." else "",
            )
            is LinkState.Retrying -> ConnState(
                "reconnecting", lastPcName,
                "Connection lost: ${s.reason}. Retrying in %.1f s.".format(s.retryInMs / 1000.0),
            )
            is LinkState.Failed -> ConnState(
                "error", "", s.reason,
                attemptsLeft = if (s.kind == FailKind.PairingFailed) s.attemptsLeft else 5,
                mismatchHost = if (s.kind == FailKind.CertMismatch) s.host else "",
            )
        }
        updateNotification(
            when (s) {
                is LinkState.Connected -> "Connected to ${s.info.pcName}"
                is LinkState.Retrying -> "Reconnecting…"
                is LinkState.Failed -> "Not connected"
                else -> "Not connected"
            },
        )
    }

    private fun startMuteWatch() {
        muteJob?.cancel()
        muteJob = scope.launch {
            while (true) {
                val now = audio.isMuted()
                if (now != muted) {
                    muted = now
                    render(connector.state.value)
                }
                delay(30_000)
            }
        }
    }

    // ------------------------------------------------------------ Android plumbing

    private fun PairInfo.toAuth(): Auth = when (this) {
        is PairInfo.Paired -> Auth.Paired(Base64.getDecoder().decode(secretB64), pinnedFp)
        is PairInfo.Qr -> Auth.Qr(Base64.getDecoder().decode(tokenB64), pinnedFp)
        is PairInfo.Pin -> Auth.Pin(pin)
    }

    @Synchronized
    private fun holdLocks() {
        releaseLocks()
        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "chiz:pen").apply {
            setReferenceCounted(false)
            acquire()
        }
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "chiz:pen").apply {
            setReferenceCounted(false)
            acquire(6 * 60 * 60 * 1000L) // a long drawing session; released on disconnect
        }
    }

    @Synchronized
    private fun releaseLocks() {
        try {
            wifiLock?.takeIf { it.isHeld }?.release()
        } catch (e: Exception) {
        }
        wifiLock = null
        try {
            wakeLock?.takeIf { it.isHeld }?.release()
        } catch (e: Exception) {
        }
        wakeLock = null
    }

    private fun createChannel() {
        val ch = NotificationChannel(CHANNEL, "Chiz", NotificationManager.IMPORTANCE_LOW)
        (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
    }

    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, ConnectionService::class.java).setAction(ACTION_DISCONNECT),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL)
            .setContentTitle("Chiz")
            .setContentText(text)
            .setSmallIcon(xyz.chiz.tablet.R.drawable.ic_logo_mono)
            .setContentIntent(open)
            .addAction(0, "Disconnect", stop)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .build()
    }

    @Volatile private var shownText = ""

    private fun updateNotification(text: String) {
        if (text == shownText) return
        shownText = text
        try {
            (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(1, notification(text))
        } catch (e: SecurityException) {
            // POST_NOTIFICATIONS denied: the foreground service still runs.
        }
    }

    companion object {
        private const val CHANNEL = "chiz"
        private const val ACTION_DISCONNECT = "xyz.chiz.tablet.DISCONNECT"
    }
}
