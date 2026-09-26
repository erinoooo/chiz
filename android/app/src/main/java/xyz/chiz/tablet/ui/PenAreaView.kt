package xyz.chiz.tablet.ui

import android.content.Context
import android.os.SystemClock
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.View
import xyz.chiz.tablet.net.PenSender
import xyz.chiz.tablet.proto.Phase
import xyz.chiz.tablet.proto.PenRecord
import xyz.chiz.tablet.proto.androidTiltToDegrees

/**
 * Pen area view (spec 6). Handles onTouchEvent + onGenericMotionEvent
 * (hover) itself; only stylus/eraser enter the pipeline; first stylus
 * pointer only; fingers/palm consumed + ignored (incl. canceled
 * ACTION_CANCEL); requestUnbufferedDispatch for pen events.
 */
class PenAreaView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    var sender: PenSender? = null
    var onRange: ((Boolean) -> Unit)? = null
    private var stylusId = -1
    private var sessionStart = 0L
    /** Test screen: report readout strings instead of sending. */
    var testReadout: ((String) -> Unit)? = null
    /** Test screen: draw a local ink trail (never sent anywhere). */
    var drawInk: Boolean = false
    private val ink = android.graphics.Path()
    private val inkPaint = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG).apply {
        color = android.graphics.Color.parseColor("#E8C547")
        style = android.graphics.Paint.Style.STROKE
        strokeWidth = 4f
        strokeCap = android.graphics.Paint.Cap.ROUND
    }

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        try {
            // API 33+: unbuffered pen dispatch (spec 5 rule 2).
            val m = View::class.java.getMethod("requestUnbufferedDispatch", Int::class.java)
            // Called per-event below with SOURCE_STYLUS.
        } catch (e: Exception) {
        }
    }

    private fun unbuffered(ev: MotionEvent) {
        try {
            val m = View::class.java.getMethod("requestUnbufferedDispatch", Int::class.java)
            m.invoke(this, ev.source)
        } catch (e: Exception) {
        }
    }

    override fun onTouchEvent(ev: MotionEvent): Boolean {
        unbuffered(ev)
        val idx = ev.actionIndex
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                if (stylusId != -1) return true
                if (!isStylus(ev, idx)) return true // consume fingers
                stylusId = ev.getPointerId(idx)
                sessionT0(ev)
                push(samplesFor(ev, idx, Phase.DOWN), true)
            }
            MotionEvent.ACTION_MOVE -> {
                val i = ev.findPointerIndex(stylusId)
                if (i < 0) return true
                push(samplesFor(ev, i, Phase.MOVE), true)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                if (ev.getPointerId(idx) != stylusId) return true
                push(samplesFor(ev, idx, Phase.UP), true)
                stylusId = -1
                onRange?.invoke(false)
            }
            MotionEvent.ACTION_CANCEL -> {
                // Stylus cancel -> CANCEL record; finger/palm cancel ignored.
                if (stylusId != -1 && ev.findPointerIndex(stylusId) >= 0) {
                    push(listOf(currentRecord(ev, ev.findPointerIndex(stylusId), Phase.CANCEL)), true)
                    stylusId = -1
                }
                onRange?.invoke(false)
            }
        }
        return true
    }

    override fun onGenericMotionEvent(ev: MotionEvent): Boolean {
        if (ev.actionMasked != MotionEvent.ACTION_HOVER_ENTER &&
            ev.actionMasked != MotionEvent.ACTION_HOVER_MOVE &&
            ev.actionMasked != MotionEvent.ACTION_HOVER_EXIT
        ) {
            return super.onGenericMotionEvent(ev)
        }
        if (ev.getToolType(0) != MotionEvent.TOOL_TYPE_STYLUS &&
            ev.getToolType(0) != MotionEvent.TOOL_TYPE_ERASER
        ) {
            return true
        }
        when (ev.actionMasked) {
            MotionEvent.ACTION_HOVER_EXIT -> {
                push(listOf(currentRecord(ev, 0, Phase.LEAVE)), false)
                onRange?.invoke(false)
            }
            else -> {
                push(listOf(currentRecord(ev, 0, Phase.HOVER)), true)
                onRange?.invoke(true)
            }
        }
        return true
    }

    private fun isStylus(ev: MotionEvent, idx: Int): Boolean {
        val t = ev.getToolType(idx)
        return t == MotionEvent.TOOL_TYPE_STYLUS || t == MotionEvent.TOOL_TYPE_ERASER
    }

    private fun sessionT0(ev: MotionEvent) {
        if (sessionStart == 0L) sessionStart = ev.eventTime
    }

    private fun push(samples: List<PenRecord>, inRange: Boolean) {
        // UI thread: hand to the sender queue, never the network itself.
        val s = sender
        if (s != null) {
            s.offer(samples, inRange)
        } else {
            testReadout?.invoke(describe(samples.lastOrNull()))
        }
        if (drawInk) {
            for (r in samples) {
                val px = r.x / 65535f * width
                val py = r.y / 65535f * height
                if (r.phase == Phase.DOWN) ink.moveTo(px, py) else ink.lineTo(px, py)
            }
            if (samples.any { it.phase == Phase.UP || it.phase == Phase.LEAVE || it.phase == Phase.CANCEL }) {
                ink.reset()
            }
            invalidate()
        }
    }

    private fun describe(r: PenRecord?): String {
        if (r == null) return "waiting for pen…"
        return "x=${r.x} y=${r.y} pressure=${r.pressure} tilt=${r.tiltX}/${r.tiltY} " +
            "distance=${r.distance} tool=${if (r.eraser) "eraser" else "pen"} " +
            "buttons=${(if (r.barrel1) "B1" else "") + (if (r.barrel2) "B2" else "")} phase=${Phase.name(r.phase)}"
    }

    override fun onDraw(canvas: android.graphics.Canvas) {
        super.onDraw(canvas)
        if (drawInk) canvas.drawPath(ink, inkPaint)
    }

    private fun samplesFor(ev: MotionEvent, idx: Int, phase: Int): List<PenRecord> {
        val out = mutableListOf<PenRecord>()
        for (h in 0 until ev.historySize) {
            out.add(recordAt(
                ev.getHistoricalX(idx, h) / width.toFloat(),
                ev.getHistoricalY(idx, h) / height.toFloat(),
                ev.getHistoricalPressure(idx, h),
                ev, idx, phase, ev.getHistoricalEventTime(h),
            ))
        }
        out.add(recordAt(ev.getX(idx) / width.toFloat(), ev.getY(idx) / height.toFloat(), ev.getPressure(idx), ev, idx, phase, ev.eventTime))
        return out
    }

    private fun currentRecord(ev: MotionEvent, idx: Int, phase: Int): PenRecord =
        recordAt(ev.getX(idx) / width.toFloat(), ev.getY(idx) / height.toFloat(), ev.getPressure(idx), ev, idx, phase, ev.eventTime)

    private fun recordAt(nx: Float, ny: Float, pressure: Float, ev: MotionEvent, idx: Int, phase: Int, t: Long): PenRecord {
        val tool = ev.getToolType(idx)
        val tilt = ev.getAxisValue(MotionEvent.AXIS_TILT, idx)
        val orient = ev.getAxisValue(MotionEvent.AXIS_ORIENTATION, idx)
        val (tx, ty) = androidTiltToDegrees(tilt, orient)
        return PenRecord(
            phase = phase,
            eraser = tool == MotionEvent.TOOL_TYPE_ERASER,
            barrel1 = ev.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0,
            barrel2 = ev.buttonState and MotionEvent.BUTTON_STYLUS_SECONDARY != 0,
            x = (nx.coerceIn(0f, 1f) * 65535).toInt(),
            y = (ny.coerceIn(0f, 1f) * 65535).toInt(),
            pressure = (pressure.coerceIn(0f, 1f) * 65535).toInt(),
            tiltX = tx.toByte(),
            tiltY = ty.toByte(),
            distance = (ev.getAxisValue(MotionEvent.AXIS_DISTANCE, idx).coerceIn(0f, 1f) * 65535).toInt(),
            tMs = t - sessionStart,
        )
    }

    fun newSession() {
        sessionStart = SystemClock.uptimeMillis()
    }
}
