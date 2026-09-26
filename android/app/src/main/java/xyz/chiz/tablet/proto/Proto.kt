package xyz.chiz.tablet.proto

import java.security.MessageDigest
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/** Spec sections 3-5: constants + HMAC/HKDF. JVM + Android compatible. */
const val PROTO_VERSION = 1
const val MAGIC = 0xC1
const val MAX_FRAME = 65536

fun hkdfSha256(ikm: ByteArray, salt: ByteArray, info: ByteArray, length: Int = 32): ByteArray {
    fun hmac(key: ByteArray, data: ByteArray): ByteArray {
        val m = Mac.getInstance("HmacSHA256")
        m.init(SecretKeySpec(key, "HmacSHA256"))
        return m.doFinal(data)
    }
    val prk = hmac(salt, ikm)
    var t = ByteArray(0)
    val okm = mutableListOf<Byte>()
    var i = 1
    while (okm.size < length) {
        t = hmac(prk, t + info + i.toByte())
        okm.addAll(t.toList())
        i++
    }
    return okm.take(length).toByteArray()
}

fun derivePenKey(deviceSecret: ByteArray, salt: ByteArray): ByteArray {
    require(deviceSecret.size == 32 && salt.size == 16)
    return hkdfSha256(deviceSecret, salt, "chiz-pen-v1".toByteArray(), 32)
}

fun computeAuth(deviceSecret: ByteArray, nonce: ByteArray, deviceId: String): String {
    val m = Mac.getInstance("HmacSHA256")
    m.init(SecretKeySpec(deviceSecret, "HmacSHA256"))
    m.update("chiz-auth-v1".toByteArray())
    m.update(nonce)
    return java.util.Base64.getEncoder().encodeToString(m.doFinal(deviceId.toByteArray()))
}

fun computePinProof(pin: String, fpSeen: ByteArray, nonce: ByteArray, deviceId: String): String {
    val m = Mac.getInstance("HmacSHA256")
    m.init(SecretKeySpec(pin.toByteArray(Charsets.US_ASCII), "HmacSHA256"))
    m.update("chiz-pair-v1".toByteArray())
    m.update(fpSeen)
    m.update(nonce)
    return java.util.Base64.getEncoder().encodeToString(m.doFinal(deviceId.toByteArray()))
}

fun constTimeEqual(a: ByteArray, b: ByteArray): Boolean = MessageDigest.isEqual(a, b)

fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }

fun hexToBytes(h: String): ByteArray =
    ByteArray(h.length / 2) { h.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
