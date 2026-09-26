package xyz.chiz.tablet.proto

/**
 * Spec 7 touch state machine. One instance per finger/button, fake-clock
 * driven. Emits: "speak", "fire", "hold_on+speak", "hold_off", "blocked".
 * A `hold` never fires on a quick tap: it engages at the rest delay and
 * releases on lift.
 */
class TouchMachine(
    val kind: String, // tap | hold | toggle
    val restDelayMs: Long = 250,
    val announceMode: String = "rest", // rest | touch
    var guardBlocked: Boolean = false,
) {
    private var downAt: Long? = null
    private var announced = false
    var held: Boolean = false
        private set

    fun down(now: Long): String? {
        downAt = now
        announced = false
        if (guardBlocked) return "blocked"
        if (announceMode == "touch") {
            announced = true
            return "speak"
        }
        return null
    }

    fun advance(now: Long): String? {
        val t0 = downAt ?: return null
        if (announced || now - t0 < restDelayMs) return null
        announced = true
        if (kind == "hold") {
            held = true
            return "hold_on+speak"
        }
        return "speak"
    }

    fun up(now: Long): String? {
        val t0 = downAt ?: return null
        downAt = null
        if (guardBlocked) return null
        val elapsedKnown = announced || now - t0 >= restDelayMs
        if (kind == "hold") {
            if (held || elapsedKnown) {
                held = false
                return "hold_off"
            }
            return null
        }
        if (announceMode == "rest") return if (!elapsedKnown) "fire" else null
        return "fire"
    }
}

/** Strip-level panic gesture state (spec: 3-finger tap = disconnect NOW). */
object PanicGesture {
    const val FINGERS = 3
    const val RECONNECT_COOLDOWN_MS = 5000L
}
