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
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.util.Base64
import androidx.core.app.NotificationCompat
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
import xyz.chiz.tablet.proto.computeAuth
import xyz.chiz.tablet.proto.computePinProof
import xyz.chiz.tablet.proto.derivePenKey
import xyz.chiz.tablet.proto.parseStripProfile
import xyz.chiz.tablet.proto.reconnectDelay
import xyz.chiz.tablet.ui.MainActivity
import java.net.InetAddress
import java.security.MessageDigest
import java.util.UUID

/** Connection status for the UI. */
data class ConnState(
    val status: String = "disconnected", // disconnected|connecting|reconnecting|connected|error
    val pcName: String = "",
    val message: String = "",
    val pairingOpen: Boolean = false,
    val attemptsLeft: Int = 5,
)

/** How this tablet will prove itself. Set by the UI before connect(). */
sealed interface PairInfo {
    data class Paired(val secretB64: String, val pinnedFp: ByteArray) : PairInfo
    data class Qr(val tokenB64: String, val pinnedFp: ByteArray) : PairInfo
    data class Pin(val pin: String) : PairInfo // fp captured from TLS, proof binds it
}

/**
 * Foreground service (type connectedDevice) owning the control socket, pen
 * sender and audio engine, so rotation never drops the session (spec 6).
 * Full handshake: challenge → pair_request/hello → welcome → profile, with
 * the 0.5/1/2/4s→5s reconnect schedule and a 5 s panic cooldown.
 */
class ConnectionService : Service() {
    inner class LocalBinder(val service: ConnectionService) : Binder()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    private val _state = MutableStateFlow(ConnState())
    val state: StateFlow<ConnState> = _state
    private val _profile = MutableStateFlow<StripProfile?>(null)
    val profile: StateFlow<StripProfile?> = _profile
    private val _toggleStates = MutableStateFlow<Map<String, Boolean>>(emptyMap())
    val toggleStates: StateFlow<Map<String, Boolean>> = _toggleStates

    val audio: AudioEngine by lazy { AudioEngine(this) }

    private var control: ControlChannel? = null
    private var sender: PenSender? = null
    private var discovery: Discovery? = null
    private var connectJob: Job? = null
    private var muteJob: Job? = null

    // Handshake state (reader-thread confined, @Volatile for UI reads).
    @Volatile private var phase = "idle"
    @Volatile private var nonce = ByteArray(0)
    @Volatile private var deviceId = ""
    @Volatile private var secretB64 = ""
    @Volatile private var penPort = 47801
    @Volatile private var sessionId = 0L
    @Volatile private var hostAddr = ""
    private var pairInfo: PairInfo? = null
    private var wantStop = false
    private var panicUntil = 0L
    private val pingSent = mutableMapOf<Int, Long>()
    @Volatile var rttMs = 0.0

    override fun onBind(intent: Intent?): IBinder = LocalBinder(this)

    override fun onCreate() {
        super.onCreate()
        createChannel()
        audio.start {}
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "DISCONNECT") {
            disconnect("bye")
            stopSelf()
            return START_NOT_STICKY
        }
        startForeground(
            1,
            notification("Connecting…"),
            android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
        )
        return START_STICKY
    }

    // ------------------------------------------------------------ connect

    fun isPanicCooldown(): Boolean = SystemClock.uptimeMillis() < panicUntil

    fun connect(host: String, port: Int, deviceIdIn: String, info: PairInfo) {
        if (isPanicCooldown()) {
            _state.value = ConnState("error", "", "Panic cooldown: wait 5 s")
            return
        }
        disconnect("")
        wantStop = false
        deviceId = deviceIdIn
        pairInfo = info
        _state.value = ConnState("connecting", host, "")
        hostAddr = host
        connectJob = scope.launch {
            var attempt = 0
            while (!wantStop && _state.value.status != "connected") {
                try {
                    runSession(host, port)
                    return@launch
                } catch (e: StopLoop) {
                    return@launch // clean disconnect / fatal error: no retry
                } catch (e: Exception) {
                    _state.value = ConnState("reconnecting", host, e.message ?: "connection failed")
                    delay((reconnectDelay(attempt++) * 1000).toLong())
                }
            }
        }
    }

    private class StopLoop : Exception()

    /** One session attempt: TLS + challenge; the message callbacks drive it. */
    private fun runSession(host: String, port: Int) {
        val info = pairInfo ?: throw StopLoop()
        val pinned: ByteArray? = when (info) {
            is PairInfo.Paired -> info.pinnedFp
            is PairInfo.Qr -> info.pinnedFp
            is PairInfo.Pin -> null // capture observed fp for the proof
        }
        if (info is PairInfo.Paired) secretB64 = info.secretB64
        phase = "wait_challenge"
        val ch = ControlChannel(host, port, pinned, ::onControlMessage, ::onDead) {
            pingSent[it] = SystemClock.uptimeMillis()
        }
        ch.connect()
        control = ch
        holdLocks()
        // Everything from here is callback-driven; this task just waits for
        // terminal states. A watchdog closes sessions stuck pre-welcome.
        val deadline = SystemClock.uptimeMillis() + 15000
        while (!wantStop && phase != "connected" && phase != "dead") {
            if (SystemClock.uptimeMillis() > deadline && phase != "connected") {
                throw Exception("handshake timeout")
            }
            Thread.sleep(200)
        }
        if (phase == "dead" || wantStop) throw StopLoop()
    }

    private fun onDead() {
        sender?.stop()
        sender = null
        releaseLocks()
        phase = "dead" // runSession's waiter retries; live sessions reconnect
        if (!wantStop && _state.value.status == "connected") {
            _state.value = _state.value.copy(status = "reconnecting", message = "connection lost")
            audio.speak("Disconnected")
            audio.earcon("disconnect")
        }
    }

    // ------------------------------------------------------------ dispatch

    private fun onControlMessage(json: String) {
        val o = try {
            JSONObject(json)
        } catch (e: Exception) {
            return // unknown/unparseable: ignore per spec
        }
        when (o.optString("t", "")) {
            "challenge" -> onChallenge(o)
            "welcome" -> onWelcome(o)
            "pair_ok" -> onPairOk(o)
            "pair_fail" -> {
                _state.value = _state.value.copy(
                    attemptsLeft = o.optInt("attempts_left", 0),
                    message = "Pairing failed: ${o.optString("code", "")}",
                )
            }
            "profile" -> onProfile(o)
            "button_state" -> {
                val id = o.optString("id", "")
                val on = o.optBoolean("on", false)
                _toggleStates.value = _toggleStates.value + (id to on)
                val label = _profile.value?.button(id)?.let { it.speak.ifEmpty { it.label } } ?: id
                audio.speak("$label ${if (on) "on" else "off"}")
                audio.earcon(if (on) "on" else "off")
            }
            "speak" -> audio.speak(o.optString("text", ""))
            "pong" -> {
                val sent = pingSent.remove(o.optInt("id", -1))
                if (sent != null) rttMs = (SystemClock.uptimeMillis() - sent).toDouble()
            }
            "error" -> {
                _state.value = ConnState("error", _state.value.pcName, o.optString("message", o.optString("code", "")))
                phase = "dead"
                wantStop = true // not_paired/busy: don't hammer
            }
            "bye" -> {
                wantStop = true
                phase = "dead"
            }
            else -> {} // unknown types ignored per spec
        }
    }

    private fun onChallenge(o: JSONObject) {
        _state.value = _state.value.copy(pairingOpen = o.optBoolean("pairing_open", false))
        nonce = Base64.decode(o.getString("nonce"), Base64.DEFAULT)
        when (val info = pairInfo) {
            is PairInfo.Paired -> {
                val auth = computeAuth(b64(secretB64), nonce, deviceId)
                control?.send(
                    JSONObject().apply {
                        put("t", "hello")
                        put("proto", 1)
                        put("device_id", deviceId)
                        put("device_name", android.os.Build.MODEL)
                        put("app_version", "1.0.0")
                        put("auth", auth)
                        put("caps", JSONObject().apply {
                            put("pressure", true); put("tilt", true); put("hover", true)
                            put("eraser", true); put("barrel", 2); put("video", false)
                        })
                        // pen_area ratio is refreshed by later `surface` msgs.
                        put("pen_area", JSONObject().apply { put("w", 2100); put("h", 1600) })
                    }.toString(),
                )
                phase = "wait_welcome"
            }
            is PairInfo.Qr -> {
                control?.send(
                    JSONObject().apply {
                        put("t", "pair_request")
                        put("proto", 1)
                        put("device_id", deviceId)
                        put("device_name", android.os.Build.MODEL)
                        put("mode", "qr")
                        put("token", info.tokenB64)
                    }.toString(),
                )
                phase = "wait_pair"
            }
            is PairInfo.Pin -> {
                val observed = control?.observedFp
                if (observed == null) {
                    _state.value = _state.value.copy(message = "no certificate observed")
                    return
                }
                val proof = computePinProof(info.pin, observed, nonce, deviceId)
                control?.send(
                    JSONObject().apply {
                        put("t", "pair_request")
                        put("proto", 1)
                        put("device_id", deviceId)
                        put("device_name", android.os.Build.MODEL)
                        put("mode", "pin")
                        put("proof", proof)
                    }.toString(),
                )
                phase = "wait_pair"
            }
            null -> {}
        }
    }

    private fun onPairOk(o: JSONObject) {
        secretB64 = o.getString("device_secret")
        // Persist via PairingStore (activity layer); fresh challenge next.
        _state.value = _state.value.copy(message = "paired")
        phase = "wait_challenge"
    }

    private fun onWelcome(o: JSONObject) {
        sessionId = o.getLong("session_id")
        penPort = o.getInt("pen_port")
        val salt = Base64.decode(o.getString("pen_key_salt"), Base64.DEFAULT)
        val key = derivePenKey(b64(secretB64), salt)
        sender?.stop()
        sender = PenSender(
            InetAddress.getByName(hostAddr), penPort, sessionId, key,
            onSeqRollover = {
                // Fresh session before seq reaches 0xFFFF0000 (spec 5).
                control?.close()
            },
        )
        phase = "connected"
        _state.value = ConnState("connected", o.optString("pc_name", hostAddr), "")
        audio.earcon("connect")
        audio.speak("Connected to ${_state.value.pcName}")
        updateNotification()
        startMuteWatch()
    }

    private fun onProfile(o: JSONObject) {
        val p = try {
            parseStripProfile(o.getJSONObject("profile"))
        } catch (e: Exception) {
            return
        }
        _profile.value = p
        _toggleStates.value = emptyMap()
        val texts = p.buttons.map { it.speak.ifEmpty { it.label } } +
            p.buttons.filter { it.kind == "toggle" }.flatMap {
                val s = it.speak.ifEmpty { it.label }
                listOf("$s on", "$s off")
            }
        audio.precache(texts)
        if (o.optString("reason", "") == "app" || o.optString("reason", "") == "manual") {
            audio.speak(p.name)
        }
    }

    // ------------------------------------------------------------ UI calls

    fun sendButton(id: String, phaseMsg: String) {
        control?.send(JSONObject().apply {
            put("t", "button")
            put("id", id)
            put("phase", phaseMsg)
        }.toString())
    }

    fun sendSurface(w: Int, h: Int) {
        control?.send(JSONObject().apply {
            put("t", "surface")
            put("w", w)
            put("h", h)
        }.toString())
    }

    /** Tablet panic gesture: stop UDP first, then bye, 5 s cooldown. */
    fun panicDisconnect() {
        sender?.stop()
        sender = null
        try {
            control?.send("""{"t":"bye"}""")
        } catch (e: Exception) {
        }
        disconnect("")
        panicUntil = SystemClock.uptimeMillis() + PanicGesture.RECONNECT_COOLDOWN_MS
        _state.value = ConnState("disconnected", "", "panic disconnect")
    }

    fun disconnect(reason: String) {
        wantStop = true
        connectJob?.cancel()
        try {
            if (reason == "bye") control?.send("""{"t":"bye"}""")
        } catch (e: Exception) {
        }
        control?.close()
        control = null
        sender?.stop()
        sender = null
        muteJob?.cancel()
        // Release local holds: fresh TouchMachines on next session.
        _toggleStates.value = emptyMap()
        if (_state.value.status == "connected") {
            _state.value = ConnState("disconnected", "", "")
            audio.speak("Disconnected")
            audio.earcon("disconnect")
        } else if (reason.isNotEmpty()) {
            _state.value = ConnState("disconnected", "", reason)
        }
        releaseLocks()
        phase = "idle"
    }

    private fun startMuteWatch() {
        muteJob?.cancel()
        muteJob = scope.launch {
            if (audio.isMuted()) {
                _state.value = _state.value.copy(message = "Volume is muted: button feedback will not be heard.")
            }
            while (_state.value.status == "connected") {
                delay(30000)
                if (audio.isMuted()) {
                    _state.value = _state.value.copy(message = "Volume is muted: button feedback will not be heard.")
                }
            }
        }
    }

    // ------------------------------------------------------------ service

    private fun b64(s: String): ByteArray = Base64.decode(s, Base64.DEFAULT)

    private fun holdLocks() {
        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        @Suppress("DEPRECATION")
        val mode = try {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } catch (e: Exception) {
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifiLock = wm.createWifiLock(mode, "chiz:pen").apply { acquire() }
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "chiz:pen").apply { acquire() }
    }

    private fun releaseLocks() {
        try {
            wifiLock?.release()
        } catch (e: Exception) {
        }
        wifiLock = null
        try {
            wakeLock?.release()
        } catch (e: Exception) {
        }
        wakeLock = null
    }

    private fun createChannel() {
        if (Build.VERSION.SDK_INT >= 26) {
            val ch = NotificationChannel("chiz", "Chiz", NotificationManager.IMPORTANCE_LOW)
            (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
        }
    }

    private fun notification(text: String): Notification {
        val pi = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, ConnectionService::class.java).setAction("DISCONNECT"),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, "chiz")
            .setContentTitle("Chiz")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setContentIntent(pi)
            .addAction(0, "Disconnect", stop)
            .setOngoing(true)
            .build()
    }

    private fun updateNotification() {
        (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .notify(1, notification("Connected to ${_state.value.pcName}"))
    }

    fun setPenRangeListener(fn: (Boolean) -> Unit) {
        // Wired to StripView button-guard via the drawing screen.
        penRangeListener = fn
    }

    private var penRangeListener: ((Boolean) -> Unit)? = null

    fun attachSender(range: (Boolean) -> Unit) {
        penRangeListener = range
    }

    override fun onDestroy() {
        scope.cancel()
        disconnect("")
        audio.stop()
        super.onDestroy()
    }

    companion object {
        /** SHA-256 hex of a DER cert, for matching QR fingerprints. */
        fun fpHex(der: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(der).joinToString("") { "%02x".format(it) }
    }
}
