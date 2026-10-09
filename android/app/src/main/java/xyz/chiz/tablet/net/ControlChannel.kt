package xyz.chiz.tablet.net

import xyz.chiz.tablet.proto.*
import java.io.ByteArrayOutputStream
import java.net.InetSocketAddress
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

/**
 * TLS 1.3 control channel (spec 3-4). Own reader thread; never the UI thread.
 *
 * - `SSLSocket`, TLS 1.3 only, TCP_NODELAY. Custom trust manager: pins the
 *   stored SHA-256 DER fingerprint (never the system store); in PIN mode it
 *   captures the observed fingerprint for the proof instead.
 * - Framing: u32 LE length + JSON, max 65536. Unknown types/fields ignored
 *   by the dispatcher (caller-side).
 * - Tablet sends `ping` 1/s; 5 s without any incoming message = dead.
 */
class ControlChannel(
    private val host: String,
    private val port: Int,
    private val pinnedFp: ByteArray?,
    private val onMessage: (String) -> Unit,
    private val onDead: () -> Unit,
    private val onPingSent: (Int) -> Unit = {},
) {
    @Volatile var observedFp: ByteArray? = null
    private var sock: SSLSocket? = null
    private var reader: Thread? = null
    private var pinger: Thread? = null
    @Volatile private var closed = false
    private var pingId = 0
    /** Per-host connect timeout ms (spec 3: 2 s per QR address). */
    var connectTimeoutMs: Int = 5000

    fun connect() {
        connectTimeoutMs = 5000
        val trust = object : X509TrustManager {
            override fun getAcceptedIssuers() = emptyArray<X509Certificate>()
            override fun checkClientTrusted(c: Array<X509Certificate>, a: String) {}
            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
                val der = chain[0].encoded
                val fp = MessageDigest.getInstance("SHA-256").digest(der)
                observedFp = fp
                val pinned = pinnedFp
                if (pinned != null && !pinMatches(fp, pinned)) {
                    throw java.security.cert.CertificateException("fingerprint mismatch")
                }
            }
        }
        val ctx = SSLContext.getInstance("TLSv1.3")
        ctx.init(null, arrayOf(trust), java.security.SecureRandom())
        val s = ctx.socketFactory.createSocket() as SSLSocket
        s.enabledProtocols = arrayOf("TLSv1.3")
        s.tcpNoDelay = true
        s.connect(InetSocketAddress(host, port), connectTimeoutMs)
        s.startHandshake()
        sock = s
        reader = Thread({ readLoop() }, "chiz-control-reader").apply { isDaemon = true; start() }
        pinger = Thread({
            while (!closed) {
                try {
                    Thread.sleep(1000)
                } catch (e: InterruptedException) {
                    return@Thread
                }
                send("""{"t":"ping","id":${++pingId}}""")
                onPingSent(pingId)
            }
        }, "chiz-pinger").apply { isDaemon = true; start() }
    }

    fun send(json: String) {
        val s = sock ?: return
        try {
            s.outputStream.write(encodeFrame(json))
            s.outputStream.flush()
        } catch (e: Exception) {
            close()
        }
    }

    /** Why the link ended, for the UI and logs ("" while alive). */
    @Volatile var deathReason: String = ""
        private set

    private fun die(reason: String) {
        if (deathReason.isEmpty()) deathReason = reason
        onDead()
    }

    private fun readLoop() {
        val s = sock ?: return
        val input = s.inputStream
        var lastRx = System.currentTimeMillis()
        val buf = ByteArrayOutputStream()
        val tmp = ByteArray(4096)
        try {
            while (!closed) {
                s.soTimeout = 500
                val n = try {
                    input.read(tmp)
                } catch (e: java.net.SocketTimeoutException) {
                    if (System.currentTimeMillis() - lastRx > 5000) {
                        die("no data from the PC for 5 s")
                        return
                    }
                    continue
                }
                if (n < 0) {
                    if (!closed) die("the PC closed the connection")
                    return
                }
                // Any bytes prove the link is alive, even a half-delivered frame.
                lastRx = System.currentTimeMillis()
                buf.write(tmp, 0, n)
                val data = buf.toByteArray()
                var off = 0
                // Drain EVERY complete frame: several often arrive in one read.
                while (true) {
                    val frame = try {
                        decodeFrame(data, off)
                    } catch (e: IllegalArgumentException) {
                        if (e.message == "oversize") {
                            die("oversize frame from the PC")
                            return
                        }
                        break // incomplete: wait for more bytes
                    }
                    // A throwing handler must not be mistaken for "incomplete"
                    // (that re-delivered the frame forever and starved lastRx).
                    try {
                        onMessage(frame.first)
                    } catch (e: Exception) {
                        System.err.println("chiz: message handler failed: $e") // logcat shows System.err
                    }
                    off += frame.second
                }
                buf.reset()
                if (off < data.size) buf.write(data, off, data.size - off)
            }
        } catch (e: Exception) {
            if (!closed) die("read error: ${e.javaClass.simpleName}: ${e.message}")
        }
    }

    fun close() {
        closed = true
        reader?.interrupt()
        pinger?.interrupt()
        try {
            sock?.close()
        } catch (e: Exception) {
        }
    }
}
