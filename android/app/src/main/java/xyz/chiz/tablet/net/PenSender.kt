package xyz.chiz.tablet.net

import android.os.SystemClock
import xyz.chiz.tablet.proto.*
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.atomic.AtomicBoolean
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * UDP pen sender (spec 5 sender rules). Own thread + bounded queue; never
 * touches the UI thread. Fed by PenAreaView with per-MotionEvent batches.
 *
 * - One datagram per MotionEvent batch; >16 records split in order.
 * - Bounded queue (64): when full, drops the oldest move/hover record.
 * - Repeats every down/up/leave/cancel twice more (+10/+20 ms).
 * - Heartbeat every 250 ms while a pen is in range (count 0 ok).
 * - Reconnect (fresh session) before seq reaches 0xFFFF0000.
 */
class PenSender(
    private val host: InetAddress,
    private val port: Int,
    private val sessionId: Long,
    private val penKey: ByteArray,
    private val onSeqRollover: () -> Unit,
) {
    private val queue = ArrayBlockingQueue<List<PenRecord>>(64)
    private val running = AtomicBoolean(true)
    private var seq: Long = 0
    private var inRange = false
    private val sock = DatagramSocket().apply { connect(host, port) }

    private val thread = Thread({
        var lastSend = SystemClock.uptimeMillis()
        while (running.get()) {
            val batch = queue.poll(50, java.util.concurrent.TimeUnit.MILLISECONDS)
            val now = SystemClock.uptimeMillis()
            if (batch != null) {
                sendBatch(batch)
                lastSend = now
            } else if (inRange && now - lastSend >= 250) {
                sendBatch(emptyList()) // heartbeat
                lastSend = now
            }
        }
    }, "chiz-pen-sender").apply { isDaemon = true; start() }

    /** Called on the UI thread: hand over one MotionEvent's samples. */
    fun offer(samples: List<PenRecord>, rangeNow: Boolean) {
        inRange = rangeNow
        for (chunk in batchSamples(samples)) {
            if (!queue.offer(chunk)) {
                // Full: drop oldest move/hover, keep the newest data.
                queue.poll()
                queue.offer(chunk)
            }
        }
    }

    private fun sendBatch(records: List<PenRecord>) {
        if (seq >= 0xFFFF0000L) {
            onSeqRollover()
            return
        }
        seq++
        val dg = encryptDatagram(penKey, sessionId, seq, records)
        sock.send(DatagramPacket(dg, dg.size))
        if (records.any { needsRepeat(it.phase) }) {
            val copy = records.toList()
            for (delay in listOf(10L, 20L)) {
                try {
                    Thread.sleep(delay)
                } catch (e: InterruptedException) {
                    return
                }
                if (!running.get()) return
                seq++
                val again = encryptDatagram(penKey, sessionId, seq, copy)
                sock.send(DatagramPacket(again, again.size))
            }
        }
    }

    fun stop() {
        running.set(false)
        thread.interrupt()
        sock.close()
    }
}
