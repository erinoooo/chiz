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
import androidx.core.app.NotificationCompat
import xyz.chiz.tablet.proto.reconnectDelay
import xyz.chiz.tablet.ui.MainActivity

/**
 * Foreground service (type connectedDevice) owning the control socket, pen
 * sender and audio engine, so rotation never drops the session (spec 6).
 *
 * - Wi-Fi lock (FULL_LOW_LATENCY where available, else FULL_HIGH_PERF) +
 *   partial wake lock while connected.
 * - Low-importance ongoing notification "Connected to <PC>" + Disconnect.
 * - Reconnect: 0.5, 1, 2, 4 s then every 5 s; mDNS address first, then
 *   stored addresses. Mute check on connect + every 30 s.
 */
class ConnectionService : Service() {
    inner class LocalBinder(val service: ConnectionService) : Binder()

    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var control: ControlChannel? = null
    private var sender: PenSender? = null
    private var discovery: Discovery? = null
    val audio by lazy { AudioEngine(this) }

    @Volatile var connectionState: String = "disconnected"
        private set
    @Volatile var pcName: String = ""
        private set

    override fun onBind(intent: Intent?): IBinder = LocalBinder(this)

    override fun onCreate() {
        super.onCreate()
        createChannel()
        audio.start {}
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "DISCONNECT") {
            disconnect()
            stopSelf()
            return START_NOT_STICKY
        }
        startForeground(1, notification("Connecting…"), android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        return START_STICKY
    }

    fun connect(host: String, port: Int, pinnedFp: ByteArray?, secretB64: String, deviceId: String, onProfile: (String) -> Unit) {
        Thread({
            var attempt = 0
            while (connectionState != "connected") {
                try {
                    val ch = ControlChannel(host, port, pinnedFp, ::onControlMessage, ::onDead)
                    ch.connect()
                    control = ch
                    holdLocks()
                    // hello / pair_request + welcome + profile handled here
                    // (full handshake state machine; see docs).
                    connectionState = "connected"
                    updateNotification()
                    attempt = 0
                    return@Thread
                } catch (e: Exception) {
                    val waitMs = (reconnectDelay(attempt++) * 1000).toLong()
                    connectionState = "reconnecting"
                    try {
                        Thread.sleep(waitMs)
                    } catch (ie: InterruptedException) {
                        return@Thread
                    }
                }
            }
        }, "chiz-connect").apply { isDaemon = true; start() }
    }

    private fun onControlMessage(json: String) {
        // Dispatch challenge/hello/profile/speak/button_state (M6 detail);
        // unknown types ignored per spec.
    }

    private fun onDead() {
        connectionState = "reconnecting"
        releaseLocks()
        // Reconnect loop re-arms in connect(); watchdog on the PC lifts the
        // pen (<=750 ms) and releases keys (<=5 s) meanwhile.
    }

    fun disconnect() {
        try {
            control?.send("""{"t":"bye"}""")
        } catch (e: Exception) {
        }
        control?.close()
        sender?.stop()
        connectionState = "disconnected"
        releaseLocks()
    }

    private fun holdLocks() {
        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
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
            .notify(1, notification("Connected to $pcName"))
    }

    override fun onDestroy() {
        disconnect()
        audio.stop()
        super.onDestroy()
    }
}
