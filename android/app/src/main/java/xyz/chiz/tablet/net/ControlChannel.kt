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
                if (pinned != null && !constTimeEqual(fp, pinned)) {
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
                        onDead()
                        return
                    }
                    continue
                }
                if (n < 0) {
                    onDead()
                    return
                }
                buf.write(tmp, 0, n)
                val bytes = buf.toByteArray()
                try {
                    val (msg, used) = decodeFrame(bytes)
                    onMessage(msg)
                    lastRx = System.currentTimeMillis()
                    buf.reset()
                    if (used < bytes.size) buf.write(bytes, used, bytes.size - used)
                } catch (e: IllegalArgumentException) {
                    if (e.message == "oversize") {
                        onDead()
                        return
                    }
                    // incomplete: keep buffering
                }
            }
        } catch (e: Exception) {
            if (!closed) onDead()
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
