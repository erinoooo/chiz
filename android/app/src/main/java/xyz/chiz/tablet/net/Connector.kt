package xyz.chiz.tablet.net

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject
import xyz.chiz.tablet.proto.computeAuth
import xyz.chiz.tablet.proto.computePinProof
import xyz.chiz.tablet.proto.constTimeEqual
import xyz.chiz.tablet.proto.derivePenKey
import xyz.chiz.tablet.proto.reconnectDelay
import xyz.chiz.tablet.proto.toHex
import java.io.IOException
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

/** How the tablet proves itself to the PC. */
sealed interface Auth {
    /** Already paired: shared secret + the PC certificate fingerprint. */
    class Paired(val secret: ByteArray, val pinnedFp: ByteArray) : Auth
    /** QR pairing: one-time token + the fingerprint printed in the QR. */
    class Qr(val token: ByteArray, val pinnedFp: ByteArray) : Auth
    /** PIN pairing: the proof binds whichever certificate we observe. */
    class Pin(val pin: String) : Auth
}

/** Where to connect: addresses are tried in order (spec 3). */
data class Target(val hosts: List<String>, val port: Int, val auth: Auth)

/** A session the PC accepted: everything the pen path needs. */
class SessionInfo(
    val host: String,
    val port: Int,
    val pcName: String,
    val sessionId: Long,
    val penPort: Int,
    val penKey: ByteArray,
)

/** Typed connection state; the UI derives its banner from this. */
sealed interface LinkState {
    data object Idle : LinkState
    data class Connecting(val host: String, val attempt: Int) : LinkState
    data class Pairing(val host: String) : LinkState
    data class Connected(val info: SessionInfo) : LinkState
    /** Lost or unreachable; the next attempt starts in [retryInMs]. */
    data class Retrying(val reason: String, val attempt: Int, val retryInMs: Long) : LinkState
    /** Retrying cannot help; the user has to act. */
    data class Failed(
        val reason: String,
        val kind: FailKind,
        val host: String = "",
        val attemptsLeft: Int = 0,
    ) : LinkState
}

enum class FailKind { CertMismatch, NotPaired, Replaced, PairingFailed }

/** What the connector needs from its owner (the Android service, or a test). */
interface ConnectorHost {
    val deviceId: String
    val deviceName: String
    /** Current pen-area size in px, sent with `hello`. */
    fun penArea(): Pair<Int, Int>
    /** Persist a new pairing (QR/PIN succeeded). */
    suspend fun savePairing(host: String, port: Int, pcName: String, fingerprint: ByteArray, secret: ByteArray)
    /** A stored pin matched only in its legacy double-hashed form: rewrite it. */
    suspend fun repairPin(stale: ByteArray, actual: ByteArray)
    fun onConnected(info: SessionInfo)
    /** Called exactly once after every [onConnected]. */
    fun onSessionEnded(reason: String)
    /** profile / button_state / speak / anything else after `welcome`. */
    fun onMessage(msg: JSONObject)
}

/** `pair_fail` from the PC. */
private class PairRejected(val code: String, val attemptsLeft: Int) : IOException("pairing rejected: $code")

/**
 * Owns at most one connection loop. [start] replaces any running loop and
 * [stop] ends it. Each loop carries a generation number and may only change
 * [state] while it is current, so a loop that is being cancelled can never
 * overwrite what its successor (or [stop]) set. Each connection attempt owns
 * its [ControlLink] and releases it in `finally`.
 */
class Connector(private val scope: CoroutineScope, private val host: ConnectorHost) {
    private val _state = MutableStateFlow<LinkState>(LinkState.Idle)
    val state: StateFlow<LinkState> = _state

    private val lock = Any()
    private var generation = 0
    private var job: Job? = null
    private val live = AtomicReference<ControlLink?>(null)
    private val pingIds = AtomicInteger()
    private val pingSentAt = ConcurrentHashMap<Int, Long>()

    @Volatile var rttMs: Long = 0
        private set

    fun start(target: Target) {
        synchronized(lock) {
            job?.cancel()
            val gen = ++generation
            _state.value = LinkState.Connecting(target.hosts.firstOrNull() ?: "", 0)
            job = scope.launch { loop(gen, target) }
        }
    }

    /** End the loop. With [bye] the PC is told first (clean disconnect). */
    fun stop(bye: Boolean, finalState: LinkState = LinkState.Idle) {
        synchronized(lock) {
            generation++
            if (bye) live.get()?.send("""{"t":"bye"}""") // flushed by finish()
            job?.cancel()
            job = null
            _state.value = finalState
        }
    }

    /** Queue a control message on the live session. Safe from any thread. */
    fun send(json: String): Boolean = live.get()?.send(json) ?: false

    private fun set(gen: Int, s: LinkState) {
        synchronized(lock) {
            if (gen == generation) _state.value = s
        }
    }

    // ------------------------------------------------------------ loop

    private sealed interface Outcome {
        /** Transport-level end: retry. [hadSession] resets the backoff. */
        class Lost(val reason: String, val hadSession: Boolean) : Outcome
        class Fatal(val state: LinkState.Failed) : Outcome
    }

    private suspend fun loop(gen: Int, initial: Target) {
        var target = initial
        var attempt = 0
        while (true) {
            val outcome = try {
                tryHosts(gen, target, attempt) { target = it }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                Outcome.Lost(e.message ?: e.javaClass.simpleName, false)
            }
            when (outcome) {
                is Outcome.Fatal -> return set(gen, outcome.state)
                is Outcome.Lost -> {
                    if (outcome.hadSession) attempt = 0
                    val wait = (reconnectDelay(attempt) * 1000).toLong()
                    set(gen, LinkState.Retrying(outcome.reason, attempt, wait))
                    attempt++
                    delay(wait)
                }
            }
        }
    }

    /** Try each address once; the first TLS success runs the session. */
    private suspend fun tryHosts(gen: Int, target: Target, attempt: Int, retarget: (Target) -> Unit): Outcome {
        var lastError = "no address to try"
        var mismatch: Outcome.Fatal? = null
        val pinned = when (val a = target.auth) {
            is Auth.Paired -> a.pinnedFp
            is Auth.Qr -> a.pinnedFp
            is Auth.Pin -> null
        }
        for (addr in target.hosts) {
            set(gen, LinkState.Connecting(addr, attempt))
            val link = try {
                ControlLink.open(scope, addr, target.port, pinned, CONNECT_TIMEOUT_MS)
            } catch (e: CertMismatchException) {
                val want = pinned?.toHex()?.take(8) ?: "?"
                val got = e.observed?.toHex()?.take(8) ?: "?"
                mismatch = mismatch ?: Outcome.Fatal(
                    LinkState.Failed(
                        "Certificate mismatch at $addr: this tablet expects $want, the device " +
                            "there showed $got. Check the Tablets page on the PC (its certificate " +
                            "id must be $want), or forget this PC and pair again.",
                        FailKind.CertMismatch,
                        host = addr,
                    ),
                )
                continue
            } catch (e: IOException) {
                lastError = "cannot reach $addr: ${e.message ?: e.javaClass.simpleName}"
                continue
            }
            // Next time, try the address that worked first.
            val reordered = target.copy(hosts = listOf(addr) + (target.hosts - addr))
            retarget(reordered)
            return session(gen, link, addr, reordered, retarget)
        }
        return mismatch ?: Outcome.Lost(lastError, false)
    }

    /** One connection, start to end: challenge, (pair), hello, session. */
    private suspend fun session(
        gen: Int,
        link: ControlLink,
        addr: String,
        target: Target,
        retarget: (Target) -> Unit,
    ): Outcome {
        var connected = false
        try {
            var challenge = link.expect("challenge", HANDSHAKE_TIMEOUT_MS)
            val paired: Auth.Paired = when (val auth = target.auth) {
                is Auth.Paired -> auth
                else -> {
                    set(gen, LinkState.Pairing(addr))
                    val secret = pair(link, auth, challenge)
                    val fp = if (auth is Auth.Qr) auth.pinnedFp else link.observedFp
                    host.savePairing(addr, target.port, challenge.optString("pc_name", addr), fp, secret)
                    challenge = link.expect("challenge", HANDSHAKE_TIMEOUT_MS)
                    Auth.Paired(secret, fp).also { retarget(target.copy(auth = it)) }
                }
            }
            val nonce = b64d(challenge.getString("nonce"))
            val (w, h) = host.penArea()
            link.send(
                JSONObject()
                    .put("t", "hello").put("proto", 1)
                    .put("device_id", host.deviceId).put("device_name", host.deviceName)
                    .put("app_version", APP_VERSION)
                    .put("auth", computeAuth(paired.secret, nonce, host.deviceId))
                    .put(
                        "caps",
                        JSONObject().put("pressure", true).put("tilt", true).put("hover", true)
                            .put("eraser", true).put("barrel", 2).put("video", false),
                    )
                    .put("pen_area", JSONObject().put("w", w).put("h", h))
                    .toString(),
            )
            val welcome = link.expect("welcome", HANDSHAKE_TIMEOUT_MS)
            if (!constTimeEqual(link.observedFp, paired.pinnedFp)) {
                // Matched only in the legacy double-hashed form: fix the record.
                host.repairPin(paired.pinnedFp, link.observedFp)
                retarget(target.copy(auth = Auth.Paired(paired.secret, link.observedFp)))
            }
            val info = SessionInfo(
                host = addr,
                port = target.port,
                pcName = challenge.optString("pc_name", addr),
                sessionId = welcome.getLong("session_id"),
                penPort = welcome.getInt("pen_port"),
                penKey = derivePenKey(paired.secret, b64d(welcome.getString("pen_key_salt"))),
            )
            live.set(link)
            connected = true
            set(gen, LinkState.Connected(info))
            host.onConnected(info)
            return runSession(link)
        } catch (e: PairRejected) {
            val text = when (e.code) {
                "bad_pin" -> "Wrong PIN. ${e.attemptsLeft} attempts left."
                "bad_token" -> "This QR code is no longer valid. Open a new pairing window on the PC."
                "closed" -> "Too many failed attempts closed the pairing window. Open it again on the PC."
                "no_window" -> "No pairing window is open on the PC. Choose \"Pair new tablet\" there first."
                else -> "Pairing failed (${e.code})."
            }
            return Outcome.Fatal(LinkState.Failed(text, FailKind.PairingFailed, addr, e.attemptsLeft))
        } catch (e: PcError) {
            return when (e.code) {
                "not_paired" -> Outcome.Fatal(
                    LinkState.Failed("The PC does not know this tablet (${e.message}). Pair again.", FailKind.NotPaired, addr),
                )
                "auth_failed" -> Outcome.Fatal(
                    LinkState.Failed("The PC rejected this tablet's key. Forget the PC and pair again.", FailKind.NotPaired, addr),
                )
                "replaced" -> Outcome.Fatal(
                    LinkState.Failed("Another connection from this tablet took over.", FailKind.Replaced, addr),
                )
                // busy and anything new: transient, retry with backoff.
                else -> Outcome.Lost("${e.message} (${e.code})", connected)
            }
        } catch (e: IOException) {
            return Outcome.Lost(e.message ?: e.javaClass.simpleName, connected)
        } finally {
            live.compareAndSet(link, null)
            link.finish("session ended")
            if (connected) host.onSessionEnded(link.closeReason ?: "ended")
        }
    }

    /** After `welcome`: ping every second, dispatch messages, enforce liveness. */
    private suspend fun runSession(link: ControlLink): Outcome = coroutineScope {
        val pinger = launch {
            while (isActive) {
                delay(PING_INTERVAL_MS)
                val id = pingIds.incrementAndGet()
                pingSentAt[id] = System.currentTimeMillis()
                link.send("""{"t":"ping","id":$id}""")
            }
        }
        try {
            while (isActive) {
                val m = link.receive(DEAD_AFTER_MS)
                    ?: return@coroutineScope Outcome.Lost("no data from the PC for ${DEAD_AFTER_MS / 1000} s", true)
                when (m.optString("t")) {
                    "pong" -> pingSentAt.remove(m.optInt("id", -1))?.let {
                        rttMs = System.currentTimeMillis() - it
                    }
                    "error" -> throw PcError(m.optString("code"), m.optString("message", m.optString("code")))
                    "bye" -> return@coroutineScope Outcome.Lost("the PC ended the session", true)
                    else -> try {
                        host.onMessage(m)
                    } catch (e: Exception) {
                        // One bad message must never take the link down.
                        System.err.println("chiz: message handler failed: $e")
                    }
                }
            }
            Outcome.Lost("stopped", true)
        } finally {
            pinger.cancel()
            pingSentAt.clear()
        }
    }

    /** Send `pair_request`; returns the new device secret. */
    private suspend fun pair(link: ControlLink, auth: Auth, challenge: JSONObject): ByteArray {
        val req = JSONObject()
            .put("t", "pair_request").put("proto", 1)
            .put("device_id", host.deviceId).put("device_name", host.deviceName)
        when (auth) {
            is Auth.Qr -> req.put("mode", "qr").put("token", Base64.getEncoder().encodeToString(auth.token))
            is Auth.Pin -> req.put("mode", "pin").put(
                "proof",
                computePinProof(auth.pin, link.observedFp, b64d(challenge.getString("nonce")), host.deviceId),
            )
            is Auth.Paired -> error("already paired")
        }
        link.send(req.toString())
        while (true) {
            val r = link.receive(HANDSHAKE_TIMEOUT_MS)
                ?: throw LinkClosedException("the PC did not answer the pairing request")
            when (r.optString("t")) {
                "pair_ok" -> return b64d(r.getString("device_secret"))
                "pair_fail" -> throw PairRejected(r.optString("code", "failed"), r.optInt("attempts_left", 0))
                "error" -> if (r.optString("code") == "not_paired") {
                    throw PairRejected("no_window", 0)
                } else {
                    throw PcError(r.optString("code"), r.optString("message", r.optString("code")))
                }
            }
        }
    }

    companion object {
        const val APP_VERSION = "1.1.0"
        const val CONNECT_TIMEOUT_MS = 2000
        const val HANDSHAKE_TIMEOUT_MS = 10_000L
        const val PING_INTERVAL_MS = 1000L
        const val DEAD_AFTER_MS = 5000L

        private fun b64d(s: String): ByteArray = Base64.getDecoder().decode(s)
    }
}
