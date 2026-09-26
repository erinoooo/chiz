package xyz.chiz.tablet.proto

import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Spec 4 framing: u32 LE length + UTF-8 JSON, max 65536. */
fun encodeFrame(json: String): ByteArray {
    val payload = json.toByteArray(Charsets.UTF_8)
    require(payload.size <= MAX_FRAME) { "frame too large" }
    val out = ByteBuffer.allocate(4 + payload.size).order(ByteOrder.LITTLE_ENDIAN)
    out.putInt(payload.size)
    out.put(payload)
    return out.array()
}

fun decodeFrame(buf: ByteArray, off: Int = 0): Pair<String, Int> {
    require(buf.size - off >= 4) { "incomplete" }
    val n = ByteBuffer.wrap(buf, off, 4).order(ByteOrder.LITTLE_ENDIAN).int
    require(n >= 0 && n <= MAX_FRAME) { "oversize" }
    require(buf.size - off >= 4 + n) { "incomplete" }
    return String(buf, off + 4, n, Charsets.UTF_8) to (4 + n)
}

/** Reconnect schedule (spec 3): 0.5, 1, 2, 4 s, then every 5 s. */
fun reconnectDelay(attempt: Int): Double = when (attempt) {
    0 -> 0.5
    1 -> 1.0
    2 -> 2.0
    3 -> 4.0
    else -> 5.0
}

/** Sender batching (spec 5 rule 1): split >16 records, oldest first. */
fun batchSamples(samples: List<PenRecord>): List<List<PenRecord>> =
    samples.chunked(16)

/** Sender rule 4: down/up/leave/cancel repeat twice (+10/+20 ms). */
fun needsRepeat(phase: Int): Boolean =
    phase == Phase.DOWN || phase == Phase.UP || phase == Phase.LEAVE || phase == Phase.CANCEL
