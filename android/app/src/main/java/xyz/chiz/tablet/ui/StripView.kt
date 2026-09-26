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

/**
 * Button strip view (spec 7). Grid of cells below a 56 dp header (connection
 * dot + Menu hold-button). Fingers only — pen events never reach here.
 *
 * - Up to 3 fingers; per-finger TouchMachine in the configured announce
 *   mode; rest delay default 250 ms; each announcement flushes speech.
 * - Button guard: touches ignored (blocked earcon) while the pen is down.
 * - 3-finger tap = panic gesture: stop UDP, close with bye, Connect screen,
 *   5 s reconnect cooldown.
 * - Every cell fully touchable; gaps cosmetic; labels >= 16 sp; toggles
 *   filled when on; labels hideable.
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
    var buttonGuardPenDown: Boolean = false
    var connected: Boolean = false

    var onFire: ((StripButton) -> Unit)? = null // send button down (+up)
    var onHold: ((StripButton, Boolean) -> Unit)? = null // hold engage/release
    var onMenu: (() -> Unit)? = null
    var onPanic: (() -> Unit)? = null

    private data class Finger(val machine: TouchMachine, var btn: StripButton?, var menuSince: Long = 0)
    private val fingers = mutableMapOf<Int, Finger>()
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        textSize = 16 * resources.displayMetrics.scaledDensity
        textAlign = Paint.Align.CENTER
    }

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

    override fun onTouchEvent(ev: MotionEvent): Boolean {
        // Panic gesture first: 3 fingers down within one event batch.
        if (ev.pointerCount >= PanicGesture.FINGERS && ev.actionMasked == MotionEvent.ACTION_POINTER_DOWN) {
            onPanic?.invoke()
            return true
        }
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                if (fingers.size >= 3) return true
                val idx = ev.actionIndex
                val id = ev.getPointerId(idx)
                if (ev.eventTime - (fingers.values.firstOrNull()?.menuSince ?: 0) > 0) {
                }
                if (ev.getY(idx) < headerH()) {
                    fingers[id] = Finger(TouchMachine("tap"), null, SystemClock.uptimeMillis())
                    return true
                }
                val b = at(ev.getX(idx), ev.getY(idx))
                val m = TouchMachine(b?.kind ?: "tap", restDelayMs, announceMode, buttonGuardPenDown)
                fingers[id] = Finger(m, b)
                when (m.down(ev.eventTime)) {
                    "blocked" -> audio?.earcon("blocked")
                    "speak" -> b?.let { audio?.speak(it.speak.ifEmpty { it.label }) }
                    "fire" -> b?.let { fire(it) }
                }
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until ev.pointerCount) {
                    val id = ev.getPointerId(i)
                    val f = fingers[id] ?: continue
                    if (f.btn == null) continue // header: menu hold handled below
                    val now = at(ev.getX(i), ev.getY(i))
                    if (now != f.btn) {
                        // Slide onto a button: speak at once in both modes;
                        // lifting afterward must NOT activate (rest) — the
                        // machine tracks this via announced=true.
                        f.btn = now
                        now?.let { audio?.speak(it.speak.ifEmpty { it.label }) }
                        // Mark announced without firing.
                        f.machine.down(ev.eventTime)
                        f.machine.advance(ev.eventTime + restDelayMs + 1)
                    } else {
                        when (f.machine.advance(ev.eventTime)) {
                            "speak" -> f.btn?.let { audio?.speak(it.speak.ifEmpty { it.label }) }
                            "hold_on+speak" -> {
                                audio?.earcon("hold_on")
                                f.btn?.let { onHold?.invoke(it, true) }
                            }
                        }
                    }
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val idx = ev.actionIndex
                val id = ev.getPointerId(idx)
                val f = fingers.remove(id) ?: return true
                // Menu hold-button: 1 s hold opens Settings/Test/Disconnect.
                if (f.btn == null && ev.getY(idx) < headerH()) {
                    if (ev.eventTime - f.menuSince >= 1000) onMenu?.invoke()
                    return true
                }
                when (f.machine.up(ev.eventTime)) {
                    "fire" -> {
                        audio?.earcon("fire")
                        f.btn?.let { fire(it) }
                    }
                    "hold_off" -> {
                        audio?.earcon("hold_off")
                        f.btn?.let { onHold?.invoke(it, false) }
                    }
                }
            }
            MotionEvent.ACTION_CANCEL -> fingers.clear()
        }
        // Menu hold check on every event.
        for ((id, f) in fingers) {
            if (f.btn == null && id >= 0) {
                val i = ev.findPointerIndex(id)
                if (i >= 0 && ev.getY(i) < headerH() &&
                    ev.eventTime - f.menuSince >= 1000 && f.menuSince > 0
                ) {
                    onMenu?.invoke()
                    f.menuSince = 0
                }
            }
        }
        invalidate()
        return true
    }

    private fun fire(b: StripButton) {
        when (b.kind) {
            "hold" -> {
                // Hold engages at rest delay only (machine guarantees).
                onHold?.invoke(b, true)
            }
            else -> onFire?.invoke(b)
        }
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        paint.color = Color.parseColor("#1b1b1f")
        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), paint)
        // Header: connection dot.
        paint.color = when {
            connected -> Color.GREEN
            else -> Color.RED
        }
        canvas.drawCircle(28 * resources.displayMetrics.density, headerH() / 2, 10f, paint)
        // Buttons.
        for (b in buttons) {
            val r = cellRect(b)
            paint.color = if (b.toggleOn) Color.parseColor("#3a6df0") else Color.parseColor("#2c2c31")
            canvas.drawRect(r, paint)
            if (showLabels) {
                canvas.drawText(b.label, r.centerX(), r.centerY() + labelPaint.textSize / 3, labelPaint)
            }
        }
    }
}
