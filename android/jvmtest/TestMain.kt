package xyz.chiz.tablet.jvmtest

import xyz.chiz.tablet.proto.*

var failures = mutableListOf<String>()

fun check(name: String, cond: Boolean, extra: String = "") {
    println((if (cond) "PASS " else "FAIL ") + name + (if (!cond && extra.isNotEmpty()) " -- $extra" else ""))
    if (!cond) failures.add(name)
}

fun main() {
    // --- section 5 test vector (byte-for-byte)
    val secret = ByteArray(32) { it.toByte() }
    val salt = ByteArray(16) { (0xa0 + it).toByte() }
    val key = derivePenKey(secret, salt)
    check("pen_key", key.toHex() == "0fcc4ff1babc2af33fbfa107f14f14632e88eafcc4624d87f11072ca74e777d9", key.toHex())
    val rec = PenRecord(Phase.DOWN, false, false, false, 0x8000, 0x4000, 0x2000, 15, -10, 0, 1234)
    check("plaintext", encodePlaintext(listOf(rec)).toHex() == "0101000080004000200ff60000d2040000")
    val dg = encryptDatagram(key, 1093482, 1, listOf(rec))
    check(
        "datagram",
        dg.toHex() == "c1006aaf1000010000008042b5e368236419bdc1a62d3f80e2e3c0b136f1c90ea1278848a0bff1f7232524",
        dg.toHex(),
    )
    val back = decryptDatagram(key, hexToBytes("c1006aaf1000010000008042b5e368236419bdc1a62d3f80e2e3c0b136f1c90ea1278848a0bff1f7232524"))
    check("decrypt", back.sessionId == 1093482L && back.seq == 1L && back.records[0].tiltY == (-10).toByte())
    try {
        val bad = hexToBytes("c1006aaf1000010000008042b5e368236419bdc1a62d3f80e2e3c0b136f1c90ea1278848a0bff1f7232524")
        bad[bad.size - 1] = (bad.last().toInt() xor 1).toByte()
        decryptDatagram(key, bad)
        check("tamper-rejected", false)
    } catch (e: Exception) {
        check("tamper-rejected", true)
    }

    // --- auth + PIN proof vectors
    val nonce = ByteArray(16) { it.toByte() }
    val dev = "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d"
    check("auth", computeAuth(secret, nonce, dev) == "DdLGZwJqsXzvI3MQKJX9vL5d9LJnGT4eYPCx6kTgps8=")
    val fp = ByteArray(32) { (0x20 + it).toByte() }
    check(
        "pin-proof",
        computePinProof("12345678", fp, nonce, dev) == "XDbvfPmZHgCAs32kb3NRxh11K7eINs+TazjCRsV116Y=",
        computePinProof("12345678", fp, nonce, dev),
    )

    // --- mapping cases (spec 13)
    check("tilt-sign", androidTiltToDegrees(0.3f, 0f).first == 0)
    val curves = listOf(
        doubleArrayOf(0.25, 0.25, 0.75, 0.75) to listOf(0.25, 0.5, 0.75),
        doubleArrayOf(0.10, 0.40, 0.50, 0.90) to listOf(0.4946, 0.7532, 0.9145),
        doubleArrayOf(0.50, 0.10, 0.90, 0.50) to listOf(0.0781, 0.2213, 0.4622),
    )
    val ps = listOf(0.25, 0.5, 0.75)
    for ((c, want) in curves) {
        for ((p, w) in ps.zip(want)) {
            val got = applyPressureCurve(c, p)
            check("curve-$p", kotlin.math.abs(got - w) <= 0.002, "$got vs $w")
        }
    }

    // --- touch machine both modes
    var m = TouchMachine("tap", announceMode = "rest")
    check("rest-tap", m.down(0) == null && m.up(100) == "fire")
    m = TouchMachine("tap", announceMode = "rest")
    m.down(0)
    check("rest-announce", m.advance(300) == "speak")
    check("rest-no-fire-after", m.up(400) == null)
    m = TouchMachine("hold", announceMode = "rest")
    m.down(0)
    check("hold-engage", m.advance(300) == "hold_on+speak")
    check("hold-release", m.up(500) == "hold_off")
    m = TouchMachine("hold", announceMode = "rest")
    m.down(0)
    check("hold-quick-never", m.up(100) == null)
    m = TouchMachine("tap", announceMode = "touch")
    check("touch-direct", m.down(0) == "speak" && m.up(50) == "fire")

    // --- framing + sender rules
    try {
        encodeFrame("x".repeat(MAX_FRAME + 1))
        check("frame-oversize", false)
    } catch (e: Exception) {
        check("frame-oversize", true)
    }
    val f = encodeFrame("""{"t":"ping","id":1}""")
    val (msg, used) = decodeFrame(f)
    check("frame-roundtrip", msg.contains("ping") && used == f.size)
    check("batch-split", batchSamples(List(20) { rec }).map { it.size } == listOf(16, 4))
    check("repeat-set", needsRepeat(Phase.DOWN) && !needsRepeat(Phase.MOVE))

    // --- QR parse
    val q = parseQrUri("chiz://pair?v=1&h=192.168.1.10,192.168.1.11&p=47800&fp=ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8&t=BwcHBwcHBwcHBwcHBwcHBw")
    check("qr", q.hosts == listOf("192.168.1.10", "192.168.1.11") && q.port == 47800 && q.fp.size == 32 && q.token.size == 16)

    // --- reconnect
    check("reconnect", (0..5).map { reconnectDelay(it) } == listOf(0.5, 1.0, 2.0, 4.0, 5.0, 5.0))

    println()
    println("FAILURES: ${if (failures.isEmpty()) "none" else failures}")
    if (failures.isNotEmpty()) kotlin.system.exitProcess(1)
}
