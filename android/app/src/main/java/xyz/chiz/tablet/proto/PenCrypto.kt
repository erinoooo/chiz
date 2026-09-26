package xyz.chiz.tablet.proto

import java.nio.ByteBuffer
import java.nio.ByteOrder
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Spec 5: AES-256-GCM datagrams. nonce = seq LE + 8 zero bytes, AAD = header. */
fun penNonce(seq: Long): ByteArray {
    val n = ByteArray(12)
    ByteBuffer.wrap(n).order(ByteOrder.LITTLE_ENDIAN).putInt((seq and 0xFFFFFFFFL).toInt())
    return n
}

fun encryptDatagram(penKey: ByteArray, sessionId: Long, seq: Long, records: List<PenRecord>): ByteArray {
    val header = ByteBuffer.allocate(10).order(ByteOrder.LITTLE_ENDIAN)
    header.put(MAGIC.toByte()); header.put(0)
    header.putInt((sessionId and 0xFFFFFFFFL).toInt())
    header.putInt((seq and 0xFFFFFFFFL).toInt())
    val h = header.array()
    val c = Cipher.getInstance("AES/GCM/NoPadding")
    c.init(Cipher.ENCRYPT_MODE, SecretKeySpec(penKey, "AES"), GCMParameterSpec(128, penNonce(seq)))
    c.updateAAD(h)
    return h + c.doFinal(encodePlaintext(records))
}

data class Decrypted(val sessionId: Long, val seq: Long, val records: List<PenRecord>)

fun decryptDatagram(penKey: ByteArray, dg: ByteArray): Decrypted {
    require(dg.size >= 10 + 1 + 16) { "too short" }
    require(dg[0].toInt() and 0xFF == MAGIC) { "bad magic" }
    val b = ByteBuffer.wrap(dg).order(ByteOrder.LITTLE_ENDIAN)
    b.get(); b.get()
    val sid = b.int.toLong() and 0xFFFFFFFFL
    val seq = b.int.toLong() and 0xFFFFFFFFL
    val c = Cipher.getInstance("AES/GCM/NoPadding")
    c.init(Cipher.DECRYPT_MODE, SecretKeySpec(penKey, "AES"), GCMParameterSpec(128, penNonce(seq)))
    c.updateAAD(dg.copyOfRange(0, 10))
    val pt = c.doFinal(dg.copyOfRange(10, dg.size))
    return Decrypted(sid, seq, decodePlaintext(pt))
}
