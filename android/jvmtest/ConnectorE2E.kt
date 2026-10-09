package xyz.chiz.tablet.jvmtest

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONObject
import xyz.chiz.tablet.net.Auth
import xyz.chiz.tablet.net.Connector
import xyz.chiz.tablet.net.ConnectorHost
import xyz.chiz.tablet.net.FailKind
import xyz.chiz.tablet.net.LinkState
import xyz.chiz.tablet.net.SessionInfo
import xyz.chiz.tablet.net.Target
import xyz.chiz.tablet.proto.hexToBytes
import xyz.chiz.tablet.proto.sha256
import xyz.chiz.tablet.proto.toHex
import java.io.File
import java.util.Base64
import java.util.concurrent.CopyOnWriteArrayList
import kotlin.system.exitProcess

/**
 * End-to-end: the tablet's real Connector + ControlLink against the real
 * PC binary (started and restarted by this program).
 *
 *   java ... ConnectorE2EKt <path to chiz-ui> <scratch dir>
 */

private const val PORT = 47900
private const val DEV = "e2e-tablet"
private val SECRET = ByteArray(32) { (it * 7 + 1).toByte() }

private var failures = 0

private fun check(name: String, ok: Boolean, detail: String = "") {
    println((if (ok) "PASS " else "FAIL ") + name + if (!ok && detail.isNotEmpty()) "  -- $detail" else "")
    if (!ok) failures++
}

private class Pc(val bin: String, val dir: File) {
    var proc: Process? = null
    val log = File(dir, "pc.log")
    private var startOffset = 0

    /** Log output of the current process only. */
    fun current(): String = log.readText().substring(startOffset)

    fun start(pairingOpen: Boolean = false) {
        File(dir, "config.json").writeText("""{"ports": {"control": $PORT, "pen": ${PORT + 1}}}""")
        val pb = ProcessBuilder(bin, "--headless")
        pb.environment().apply {
            put("CHIZ_DATA_DIR", dir.absolutePath)
            put("CHIZ_CONFIG", File(dir, "config.json").absolutePath)
            put("CHIZ_TEST_PAIR", "$DEV:" + Base64.getEncoder().encodeToString(SECRET))
            if (pairingOpen) put("CHIZ_PAIRING", "open") else remove("CHIZ_PAIRING")
            remove("DISPLAY")
            remove("WAYLAND_DISPLAY")
        }
        pb.redirectErrorStream(true).redirectOutput(ProcessBuilder.Redirect.appendTo(log))
        startOffset = if (log.exists()) log.length().toInt() else 0
        proc = pb.start()
        val t0 = System.currentTimeMillis()
        while (System.currentTimeMillis() - t0 < 10_000) {
            if (current().contains("listening: control TCP $PORT")) return
            Thread.sleep(50)
        }
        error("PC did not start; log:\n" + log.readText())
    }

    fun stop() {
        proc?.destroy()
        proc?.waitFor()
        proc = null
    }

    fun fingerprint(): ByteArray =
        hexToBytes(Regex("certificate id: ([0-9a-f]{64})").findAll(current()).last().groupValues[1])

    fun qrUri(): String = Regex("pairing QR: (chiz://\\S+)").findAll(current()).last().groupValues[1]
}

private class TestHost(override val deviceId: String = DEV) : ConnectorHost {
    override val deviceName = "JVM e2e"
    val events = CopyOnWriteArrayList<String>()
    val messages = CopyOnWriteArrayList<JSONObject>()
    var savedFp: ByteArray? = null
    var savedSecret: ByteArray? = null
    var repaired: Pair<ByteArray, ByteArray>? = null

    override fun penArea() = 2100 to 1600
    override suspend fun savePairing(host: String, port: Int, pcName: String, fingerprint: ByteArray, secret: ByteArray) {
        savedFp = fingerprint
        savedSecret = secret
        events += "saved"
    }
    override suspend fun repairPin(stale: ByteArray, actual: ByteArray) {
        repaired = stale to actual
        events += "repaired"
    }
    override fun onConnected(info: SessionInfo) {
        events += "connected"
    }
    override fun onSessionEnded(reason: String) {
        events += "ended: $reason"
    }
    override fun onMessage(msg: JSONObject) {
        messages += msg
    }
}

private suspend fun Connector.awaitState(ms: Long, pred: (LinkState) -> Boolean): LinkState? =
    withTimeoutOrNull(ms) { state.first(pred) }

fun main(args: Array<String>) {
    val ok = runBlocking { scenarios(args) }
    exitProcess(if (ok) 0 else 1)
}

private suspend fun scenarios(args: Array<String>): Boolean {
    val bin = args[0]
    val dir = File(args[1]).apply { deleteRecursively(); mkdirs() }
    val pc = Pc(bin, dir)
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    try {
        pc.start()
        val fp = pc.fingerprint()
        val target = { auth: Auth -> Target(listOf("127.0.0.1"), PORT, auth) }

        // 1. Paired connect, survives well past the 5 s liveness window, and
        //    messages sent from another thread (like Android's main thread) work.
        run {
            val host = TestHost()
            val c = Connector(scope, host)
            c.start(target(Auth.Paired(SECRET, fp)))
            check("paired: connects", c.awaitState(5000) { it is LinkState.Connected } != null, "${c.state.value}")
            check("paired: profile arrives", withTimeoutOrNull(3000) {
                while (host.messages.none { it.optString("t") == "profile" }) delay(20)
                true
            } == true)
            val sent = Thread { c.send("""{"t":"button","id":"eraser","phase":"down"}""") }
            sent.start(); sent.join()
            c.send("""{"t":"button","id":"eraser","phase":"up"}""")
            check("paired: button from another thread gets button_state", withTimeoutOrNull(3000) {
                while (host.messages.none { it.optString("t") == "button_state" }) delay(20)
                true
            } == true)
            delay(8000)
            check(
                "paired: still connected after 8 s (pings keep it alive)",
                c.state.value is LinkState.Connected && host.events.count { it == "connected" } == 1,
                "${c.state.value} ${host.events}",
            )
            check("paired: rtt measured", c.rttMs in 0..500, "${c.rttMs}")
            c.stop(bye = true)
            delay(500)
            check("stop(bye): PC saw bye", pc.log.readText().contains("tablet said bye"))
            check("stop: session ended callback once", host.events.count { it.startsWith("ended") } == 1, "${host.events}")
            check("stop: state idle", c.state.value == LinkState.Idle)
        }

        // 2. Records saved by old builds (double-hashed pin) still connect and get repaired.
        run {
            val host = TestHost()
            val c = Connector(scope, host)
            c.start(target(Auth.Paired(SECRET, sha256(fp))))
            check("legacy pin: connects", c.awaitState(5000) { it is LinkState.Connected } != null, "${c.state.value}")
            check(
                "legacy pin: repaired to the real fingerprint",
                host.repaired?.second?.contentEquals(fp) == true,
            )
            c.stop(bye = true)
        }

        // 3. A different certificate fails permanently, with both ids in the text.
        run {
            val c = Connector(scope, TestHost())
            c.start(target(Auth.Paired(SECRET, sha256(byteArrayOf(1)))))
            val s = c.awaitState(5000) { it is LinkState.Failed } as? LinkState.Failed
            check("wrong pin: fails as mismatch", s?.kind == FailKind.CertMismatch, "$s")
            check("wrong pin: names the PC's id", s?.reason?.contains(fp.toHex().take(8)) == true, "${s?.reason}")
            c.stop(bye = false)
        }

        // 4. Unknown tablet: permanent, not a reconnect loop.
        run {
            val c = Connector(scope, TestHost(deviceId = "nobody"))
            c.start(target(Auth.Paired(ByteArray(32), fp)))
            val s = c.awaitState(5000) { it is LinkState.Failed } as? LinkState.Failed
            check("unknown tablet: fails as not paired", s?.kind == FailKind.NotPaired, "$s")
            c.stop(bye = false)
        }

        // 5. The PC restarts: the tablet reconnects on its own.
        run {
            val host = TestHost()
            val c = Connector(scope, host)
            c.start(target(Auth.Paired(SECRET, fp)))
            check("restart: connected", c.awaitState(5000) { it is LinkState.Connected } != null)
            pc.stop()
            val lost = c.awaitState(7000) { it is LinkState.Retrying }
            check("restart: notices the PC is gone", lost != null, "${c.state.value}")
            pc.start()
            check(
                "restart: reconnects by itself",
                c.awaitState(15000) { it is LinkState.Connected } != null,
                "${c.state.value}",
            )
            check("restart: one ended per connected", host.events.count { it == "connected" } == host.events.count { it.startsWith("ended") } + 1, "${host.events}")
            c.stop(bye = true)
        }

        // 6. Same tablet connecting twice: the newer wins, the older stops (no ping-pong).
        run {
            val a = Connector(scope, TestHost())
            a.start(target(Auth.Paired(SECRET, fp)))
            check("replace: first connects", a.awaitState(5000) { it is LinkState.Connected } != null)
            val b = Connector(scope, TestHost())
            b.start(target(Auth.Paired(SECRET, fp)))
            check("replace: second connects", b.awaitState(5000) { it is LinkState.Connected } != null)
            val aState = a.awaitState(3000) { it is LinkState.Failed } as? LinkState.Failed
            check("replace: first stops as replaced", aState?.kind == FailKind.Replaced, "${a.state.value}")
            delay(3000)
            check("replace: second stays connected", b.state.value is LinkState.Connected, "${b.state.value}")
            b.stop(bye = true)
        }

        // 7. QR pairing end to end: the saved fingerprint is the REAL one.
        run {
            pc.stop()
            pc.start(pairingOpen = true)
            val uri = pc.qrUri()
            val q = xyz.chiz.tablet.proto.parseQrUri(uri)
            val host = TestHost(deviceId = "qr-tablet")
            val c = Connector(scope, host)
            c.start(Target(listOf("127.0.0.1"), PORT, Auth.Qr(q.token, q.fp)))
            check("qr: pairs and connects", c.awaitState(8000) { it is LinkState.Connected } != null, "${c.state.value}")
            check("qr: saved fingerprint is the real certificate fingerprint", host.savedFp?.contentEquals(fp) == true)
            c.stop(bye = true)
            delay(300)
            // Reconnect with what was saved: must work (the old double-hash bug failed here).
            val c2 = Connector(scope, TestHost(deviceId = "qr-tablet"))
            c2.start(target(Auth.Paired(host.savedSecret!!, host.savedFp!!)))
            check("qr: saved pairing reconnects", c2.awaitState(5000) { it is LinkState.Connected } != null, "${c2.state.value}")
            c2.stop(bye = true)
        }
    } finally {
        pc.stop()
    }
    scope.coroutineContext[kotlinx.coroutines.Job]?.cancel()
    println(if (failures == 0) "E2E: ALL PASSED" else "E2E: $failures FAILED")
    return failures == 0
}
