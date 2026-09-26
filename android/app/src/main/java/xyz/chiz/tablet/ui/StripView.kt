package xyz.chiz.tablet.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.os.SystemClock
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.View
import xyz.chiz.tablet.net.AudioEngine
import xyz.chiz.tablet.proto.PanicGesture
import xyz.chiz.tablet.proto.TouchMachine

/** Button model from the PC `profile` message. */
data class StripButton(
    val id: String,
    val label: String,
    val speak: String,
    val kind: String, // tap | hold | toggle
    val col: Int,
    val row: Int,
    val colspan: Int = 1,
    val rowspan: Int = 1,
    var toggleOn: Boolean = false,
)

fun xyz.chiz.tablet.proto.ButtonDef.toStrip() = StripButton(id, label, speak, kind, col, row, colspan, rowspan)

/**
 * Button strip view (spec 7). Grid below a 56 dp header (connection dot +
 * Menu hold-button). Fingers only — pen events never reach here.
 *
 * Engine: one [TouchMachine] per finger slot, driven by touch events plus a
 * 100 ms ticker (rest-delay announcements and the 1 s menu hold both fire
 * even when the finger doesn't move). Up to 3 fingers.
 *
 * - Button guard: while the pen is down, touches only play `blocked`.
 * - 3-finger tap = panic: [onPanic].
 * - Every cell fully touchable; gaps cosmetic; labels >= 16 sp; toggles
 *   filled when on ([setToggles] from the service).
 */
class StripView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    var cols: Int = 1
    var rows: Int = 8
    var buttons: List<StripButton> = emptyList()
    var audio: AudioEngine? = null
    var announceMode: String = "rest"
    var restDelayMs: Long = 250
    var showLabels: Boolean = true
    private var guardPenDown: Boolean = false
    var connected: Boolean = false

    var onFire: ((StripButton) -> Unit)? = null // tap/toggle: service sends down+up
    var onHold: ((StripButton, Boolean) -> Unit)? = null // hold engage/release
    var onMenu: (() -> Unit)? = null
    var onPanic: (() -> Unit)? = null

    private data class Slot(
        var machine: TouchMachine,
        var button: StripButton?,
        var isMenu: Boolean,
        var menuDownAt: Long = 0L,
        var menuFired: Boolean = false,
    )

    private val slots = mutableMapOf<Int, Slot>()

    private val ticker = object : Runnable {
        override fun run() {
            val now = SystemClock.uptimeMillis()
            for ((_, s) in slots) {
                if (s.isMenu) {
                    if (!s.menuFired && s.menuDownAt > 0 && now - s.menuDownAt >= 1000) {
                        s.menuFired = true
                        onMenu?.invoke()
                    }
                    continue
                }
                when (s.machine.advance(now)) {
                    "speak" -> s.button?.let { speakOf(it) }
                    "hold_on+speak" -> {
                        audio?.earcon("hold_on")
                        s.button?.let { onHold?.invoke(it, true) }
                    }
                }
            }
            if (slots.isNotEmpty()) postDelayed(this, 100)
        }
    }

    private fun kickTicker() {
        removeCallbacks(ticker)
        if (slots.isNotEmpty()) postDelayed(ticker, 100)
    }

    private fun speakOf(b: StripButton) = audio?.speak(b.speak.ifEmpty { b.label })

    private fun headerH(): Float = 56 * resources.displayMetrics.density

    private fun cellRect(b: StripButton): RectF {
        val top = headerH()
        val cw = width.toFloat() / cols
        val ch = (height - top) / rows
        return RectF(b.col * cw, top + b.row * ch, (b.col + b.colspan) * cw, top + (b.row + b.rowspan) * ch)
    }

    private fun at(x: Float, y: Float): StripButton? {
        if (y < headerH()) return null
        return buttons.firstOrNull { cellRect(it).contains(x, y) }
    }

    private fun freshMachine(b: StripButton?, now: Long): TouchMachine {
        val m = TouchMachine(b?.kind ?: "tap", restDelayMs, announceMode, guardPenDown)
        m.down(now) // return handled by caller
        return m
    }

    override fun onTouchEvent(ev: MotionEvent): Boolean {
        if (ev.pointerCount >= PanicGesture.FINGERS && ev.actionMasked == MotionEvent.ACTION_POINTER_DOWN) {
            releaseAllHolds()
            slots.clear()
            removeCallbacks(ticker)
            onPanic?.invoke()
            invalidate()
            return true
        }
        val now = ev.eventTime
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                if (slots.size >= 3) return true
                val idx = ev.actionIndex
                val id = ev.getPointerId(idx)
                if (ev.getY(idx) < headerH()) {
                    slots[id] = Slot(TouchMachine("tap"), null, true, menuDownAt = now)
                } else {
                    val b = at(ev.getX(idx), ev.getY(idx))
                    val m = TouchMachine(b?.kind ?: "tap", restDelayMs, announceMode, guardPenDown)
                    slots[id] = Slot(m, b, false)
                    when (m.down(now)) {
                        "blocked" -> audio?.earcon("blocked")
                        "speak" -> b?.let { speakOf(it) }
                        "fire" -> b?.let { fireTap(it) }
                    }
                }
                kickTicker()
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until ev.pointerCount) {
                    val id = ev.getPointerId(i)
                    val s = slots[id] ?: continue
                    if (s.isMenu) continue
                    val cur = at(ev.getX(i), ev.getY(i))
                    if (cur !== s.button) {
                        slideTo(s, cur, now)
                    } else {
                        when (s.machine.advance(now)) {
                            "speak" -> s.button?.let { speakOf(it) }
                            "hold_on+speak" -> {
                                audio?.earcon("hold_on")
                                s.button?.let { onHold?.invoke(it, true) }
                            }
                        }
                    }
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val id = ev.getPointerId(ev.actionIndex)
                val s = slots.remove(id) ?: return true
                if (s.isMenu) {
                    if (!s.menuFired && now - s.menuDownAt >= 1000) onMenu?.invoke()
                } else {
                    when (s.machine.up(now)) {
                        "fire" -> {
                            audio?.earcon("fire")
                            s.button?.let { fireTap(it) }
                        }
                        "hold_off" -> {
                            audio?.earcon("hold_off")
                            s.button?.let { onHold?.invoke(it, false) }
                        }
                    }
                }
                kickTicker()
            }
            MotionEvent.ACTION_CANCEL -> {
                releaseAllHolds()
                slots.clear()
                removeCallbacks(ticker)
            }
        }
        invalidate()
        return true
    }

    /**
     * Finger slid to another button: speak at once in both modes. The fresh
     * machine is pre-announced in rest mode (lift must NOT fire) and left
     * live in touch mode (lift fires the new button). A held `hold` is
     * released first.
     */
    private fun slideTo(s: Slot, cur: StripButton?, now: Long) {
        if (s.machine.held) {
            audio?.earcon("hold_off")
            s.button?.let { onHold?.invoke(it, false) }
        }
        val m = TouchMachine(cur?.kind ?: "tap", restDelayMs, announceMode, guardPenDown)
        m.down(now)
        s.machine = m
        s.button = cur
        if (cur != null) {
            speakOf(cur)
            if (announceMode == "rest") m.advance(now + restDelayMs + 1) // pre-announce: lift won't fire
        }
    }

    private fun fireTap(b: StripButton) {
        if (b.kind == "hold") {
            // Hold engages at rest delay only; a tap-down fire here would
            // violate spec 7, so the machine never produces it.
            return
        }
        onFire?.invoke(b)
    }

    private fun releaseAllHolds() {
        for ((_, s) in slots) {
            if (s.machine.held) {
                audio?.earcon("hold_off")
                s.button?.let { onHold?.invoke(it, false) }
            }
        }
    }

    /** Pen touched/lifted the pen area: guard blocks new touches; a pen-down
     * releases in-flight holds so a palm can't stick a modifier. */
    fun setPenDown(down: Boolean) {
        guardPenDown = down
        if (down) {
            releaseAllHolds()
            slots.clear()
            removeCallbacks(ticker)
            invalidate()
        }
    }

    fun setToggles(states: Map<String, Boolean>) {
        for (b in buttons) b.toggleOn = states[b.id] == true
        invalidate()
    }

    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        textSize = 16 * resources.displayMetrics.scaledDensity
        textAlign = Paint.Align.CENTER
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        paint.color = Color.parseColor("#141318")
        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), paint)
        paint.color = if (connected) Color.parseColor("#4CAF50") else Color.parseColor("#F44336")
        canvas.drawCircle(28 * resources.displayMetrics.density, headerH() / 2, 10f, paint)
        for (b in buttons) {
            val r = cellRect(b)
            paint.color = if (b.toggleOn) Color.parseColor("#3A6DF0") else Color.parseColor("#232329")
            val rad = 12f
            canvas.drawRoundRect(r.left + 4, r.top + 4, r.right - 4, r.bottom - 4, rad, rad, paint)
            if (showLabels) {
                canvas.drawText(b.label, r.centerX(), r.centerY() + labelPaint.textSize / 3, labelPaint)
            }
        }
    }
}
