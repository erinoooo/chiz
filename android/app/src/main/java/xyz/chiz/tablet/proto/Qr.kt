package xyz.chiz.tablet.proto

/** Spec 3 QR codec. Manual parse (no android.net.Uri) so JVM tests cover it. */
data class QrPairing(val hosts: List<String>, val port: Int, val fp: ByteArray, val token: ByteArray)

fun parseQrUri(uri: String): QrPairing {
    val q = uri.removePrefix("chiz://pair?")
    require(uri.startsWith("chiz://pair?")) { "bad QR scheme" }
    val m = q.split('&').associate { it.substringBefore('=') to it.substringAfter('=', "") }
    require(m["v"] == "1") { "bad QR version" }
    val hosts = (m["h"] ?: error("missing h")).split(',')
    require(hosts.isNotEmpty() && hosts.all { it.isNotEmpty() }) { "bad hosts" }
    val port = (m["p"] ?: error("missing p")).toInt()
    fun unb64(s: String): ByteArray {
        var t = s.replace('-', '+').replace('_', '/')
        t += "=".repeat((4 - t.length % 4) % 4)
        return java.util.Base64.getDecoder().decode(t)
    }
    val fp = unb64(m["fp"] ?: error("missing fp"))
    require(fp.size == 32) { "bad fp length" }
    val token = unb64(m["t"] ?: error("missing t"))
    require(token.size == 16) { "bad token length" }
    return QrPairing(hosts, port, fp, token)
}
