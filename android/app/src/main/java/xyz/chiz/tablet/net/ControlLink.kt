package xyz.chiz.tablet.net

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ClosedReceiveChannelException
import kotlinx.coroutines.launch
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONObject
import xyz.chiz.tablet.proto.decodeFrame
import xyz.chiz.tablet.proto.encodeFrame
import xyz.chiz.tablet.proto.pinMatches
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.InetSocketAddress
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

/** The PC presented a different certificate than the one this tablet pinned. */
class CertMismatchException(val observed: ByteArray?) : IOException("fingerprint mismatch")

/** The link ended; [message] says why in words a user can act on. */
class LinkClosedException(message: String) : IOException(message)

/**
 * One TLS 1.3 control connection (spec 3, 4), with no threads of its own
 * to manage and no shared state:
 *
 * - a reader coroutine turns frames into [JSONObject]s on [incoming];
 * - a writer coroutine is the ONLY code that writes to the socket.
 *   [send] just queues, so it is safe and non-blocking from any thread,
 *   including the Android main thread (where a direct socket write throws
 *   NetworkOnMainThreadException).
 *
 * The first failure on either side closes the socket and ends both; the
 * reason is kept in [closeReason] and surfaced by [receive].
 */
class ControlLink private constructor(
    private val sock: SSLSocket,
    /** SHA-256 of the certificate the PC presented. */
    val observedFp: ByteArray,
    scope: CoroutineScope,
) {
    private val incoming = Channel<JSONObject>(Channel.UNLIMITED)
    private val outgoing = Channel<String>(Channel.UNLIMITED)

    @Volatile var closeReason: String? = null
        private set

    private val scopeRef = scope

    private val job: Job = scope.launch(Dispatchers.IO) {
        launch { readLoop() }
        launch { writeLoop() }
    }

    /** Queue a message. Returns false once the link is closed. */
    fun send(json: String): Boolean = outgoing.trySend(json).isSuccess

    /**
     * Next message, or null if nothing arrived within [timeoutMs].
     * Throws [LinkClosedException] (with the reason) once the link is down.
     */
    suspend fun receive(timeoutMs: Long): JSONObject? = try {
        withTimeoutOrNull(timeoutMs) { incoming.receive() }
    } catch (e: ClosedReceiveChannelException) {
        throw LinkClosedException(closeReason ?: "connection closed")
    }

    /** Next message of type [t], skipping others; throws on timeout/close. */
    suspend fun expect(t: String, timeoutMs: Long): JSONObject {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            val left = deadline - System.currentTimeMillis()
            if (left <= 0) throw LinkClosedException("the PC did not answer in time")
            val m = receive(left) ?: continue
            when (m.optString("t")) {
                t -> return m
                "error" -> throw PcError(m.optString("code"), m.optString("message", m.optString("code")))
            }
        }
    }

    /**
     * Graceful end: everything already queued (e.g. `bye`) is written, then
     * the socket closes. A stuck peer cannot hold it open past 500 ms.
     */
    fun finish(reason: String) {
        if (closeReason == null) closeReason = reason
        outgoing.close() // the writer drains what is queued, then closes
        scopeRef.launch {
            kotlinx.coroutines.delay(500)
            close(reason)
        }
    }

    /** Immediate end. Safe to call repeatedly and from any thread. */
    fun close(reason: String) {
        if (closeReason == null) closeReason = reason
        outgoing.close()
        incoming.close()
        try {
            sock.close() // unblocks the reader
        } catch (e: IOException) {
        }
        job.cancel()
    }

    private suspend fun readLoop() {
        val input = sock.inputStream
        val buf = ByteArrayOutputStream()
        val tmp = ByteArray(8192)
        try {
            while (true) {
                val n = runInterruptible { input.read(tmp) }
                if (n < 0) return close("the PC closed the connection")
                buf.write(tmp, 0, n)
                val data = buf.toByteArray()
                var off = 0
                // Drain every complete frame: several often arrive together.
                while (true) {
                    val (text, used) = try {
                        decodeFrame(data, off)
                    } catch (e: IllegalArgumentException) {
                        if (e.message == "oversize") return close("oversize frame from the PC")
                        break // incomplete: wait for more bytes
                    }
                    val obj = try {
                        JSONObject(text)
                    } catch (e: Exception) {
                        return close("malformed frame from the PC")
                    }
                    incoming.trySend(obj)
                    off += used
                }
                buf.reset()
                if (off < data.size) buf.write(data, off, data.size - off)
            }
        } catch (e: IOException) {
            close("connection lost: ${e.message ?: e.javaClass.simpleName}")
        }
    }

    private suspend fun writeLoop() {
        val out = sock.outputStream
        try {
            for (msg in outgoing) {
                runInterruptible {
                    out.write(encodeFrame(msg))
                    out.flush()
                }
            }
            close(closeReason ?: "closed") // finish(): queue drained
        } catch (e: IOException) {
            close("send failed: ${e.message ?: e.javaClass.simpleName}")
        }
    }

    companion object {
        /**
         * Connect and finish the TLS 1.3 handshake. With [pinned] set, any
         * other certificate fails with [CertMismatchException]; without it
         * (PIN pairing) the observed fingerprint is captured for the proof.
         */
        suspend fun open(
            scope: CoroutineScope,
            host: String,
            port: Int,
            pinned: ByteArray?,
            connectTimeoutMs: Int,
        ): ControlLink = withContext(Dispatchers.IO) {
            var observed: ByteArray? = null
            val trust = object : X509TrustManager {
                override fun getAcceptedIssuers() = emptyArray<X509Certificate>()
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) =
                    throw CertificateException("client certificates are not used")
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
                    val fp = MessageDigest.getInstance("SHA-256").digest(chain[0].encoded)
                    observed = fp
                    if (pinned != null && !pinMatches(fp, pinned)) {
                        throw CertificateException("fingerprint mismatch")
                    }
                }
            }
            val ctx = SSLContext.getInstance("TLSv1.3")
            ctx.init(null, arrayOf(trust), java.security.SecureRandom())
            val s = ctx.socketFactory.createSocket() as SSLSocket
            try {
                s.enabledProtocols = arrayOf("TLSv1.3")
                s.tcpNoDelay = true
                s.keepAlive = true
                s.connect(InetSocketAddress(host, port), connectTimeoutMs)
                s.soTimeout = connectTimeoutMs * 2 // bounds the handshake only
                s.startHandshake()
                s.soTimeout = 0 // reads block; liveness is enforced by receive()
            } catch (e: Exception) {
                try {
                    s.close()
                } catch (_: IOException) {
                }
                if (generateSequence<Throwable>(e) { it.cause }.any { it.message?.contains("fingerprint mismatch") == true }) {
                    throw CertMismatchException(observed)
                }
                throw if (e is IOException) e else IOException(e.message, e)
            }
            ControlLink(s, observed ?: throw IOException("no certificate observed"), scope)
        }
    }
}

/** An `error` message from the PC (spec 4); the PC closes right after it. */
class PcError(val code: String, message: String) : IOException(message)
