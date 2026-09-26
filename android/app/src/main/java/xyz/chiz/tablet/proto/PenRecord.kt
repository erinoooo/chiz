package xyz.chiz.tablet.proto

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Spec 5: pen record phases + 16-byte codec. */
object Phase {
    const val HOVER = 0
    const val DOWN = 1
    const val MOVE = 2
    const val UP = 3
    const val LEAVE = 4
    const val CANCEL = 5
    fun name(v: Int) = when (v) {
        HOVER -> "hover"; DOWN -> "down"; MOVE -> "move"; UP -> "up"
        LEAVE -> "leave"; else -> "cancel"
    }
    fun value(n: String) = when (n) {
        "hover" -> HOVER; "down" -> DOWN; "move" -> MOVE; "up" -> UP
        "leave" -> LEAVE; else -> CANCEL
    }
}

data class PenRecord(
    val phase: Int,
    val eraser: Boolean = false,
    val barrel1: Boolean = false,
    val barrel2: Boolean = false,
    val x: Int = 0,
    val y: Int = 0,
    val pressure: Int = 0,
    val tiltX: Byte = 0,
    val tiltY: Byte = 0,
    val distance: Int = 0,
    val tMs: Long = 0,
)

fun encodeRecord(r: PenRecord): ByteArray {
    var flags = r.phase and 0x07
    if (r.eraser) flags = flags or 0x08
    if (r.barrel1) flags = flags or 0x10
    if (r.barrel2) flags = flags or 0x20
    val b = ByteBuffer.allocate(16).order(ByteOrder.LITTLE_ENDIAN)
    b.put(flags.toByte()); b.put(0)
    b.putShort(r.x.toShort()); b.putShort(r.y.toShort()); b.putShort(r.pressure.toShort())
    b.put(r.tiltX); b.put(r.tiltY)
    b.putShort(r.distance.toShort())
    b.putInt((r.tMs and 0xFFFFFFFFL).toInt())
    return b.array()
}

fun decodeRecord(buf: ByteArray, off: Int = 0): PenRecord {
    val b = ByteBuffer.wrap(buf, off, 16).order(ByteOrder.LITTLE_ENDIAN)
    val flags = b.get().toInt() and 0xFF
    b.get()
    return PenRecord(
        phase = flags and 0x07,
        eraser = flags and 0x08 != 0,
        barrel1 = flags and 0x10 != 0,
        barrel2 = flags and 0x20 != 0,
        x = b.short.toInt() and 0xFFFF,
        y = b.short.toInt() and 0xFFFF,
        pressure = b.short.toInt() and 0xFFFF,
        tiltX = b.get(), tiltY = b.get(),
        distance = b.short.toInt() and 0xFFFF,
        tMs = b.int.toLong() and 0xFFFFFFFFL,
    )
}

fun encodePlaintext(records: List<PenRecord>): ByteArray {
    require(records.size <= 16)
    val out = ByteArray(1 + 16 * records.size)
    out[0] = records.size.toByte()
    records.forEachIndexed { i, r -> encodeRecord(r).copyInto(out, 1 + 16 * i) }
    return out
}

fun decodePlaintext(data: ByteArray): List<PenRecord> {
    val count = data[0].toInt() and 0xFF
    require(count <= 16 && data.size == 1 + 16 * count)
    return (0 until count).map { decodeRecord(data, 1 + 16 * it) }
}
