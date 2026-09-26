package xyz.chiz.tablet.proto

import kotlin.math.atan
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.tan

/**
 * Spec 5 tilt conversion. AXIS_TILT = radians from vertical (0 =
 * perpendicular, PI/2 = flat); AXIS_ORIENTATION = radians, 0 = pointing up,
 * positive clockwise. Single function so one sign flip fixes device
 * variance; verify with AT-5 (right -> X+, bottom -> Y+).
 */
fun androidTiltToDegrees(tiltRad: Float, orientRad: Float): Pair<Int, Int> {
    val tilt = tiltRad.coerceIn(0f, Math.toRadians(89.0).toFloat()).toDouble()
    val orient = orientRad.toDouble()
    val tx = Math.toDegrees(atan(tan(tilt) * sin(orient))).toInt()
    val ty = Math.toDegrees(-atan(tan(tilt) * cos(orient))).toInt()
    return tx.coerceIn(-90, 90) to ty.coerceIn(-90, 90)
}

/** Spec 8 mapping helpers (tablet mirrors the math for the test screen). */
fun rotateUv(x: Double, y: Double, rotation: Int): Pair<Double, Double> = when (rotation) {
    90 -> (1 - y) to x
    180 -> (1 - x) to (1 - y)
    270 -> y to (1 - x)
    else -> x to y
}

private fun bezier(c: DoubleArray, t: Double): Pair<Double, Double> {
    val mt = 1 - t
    val x = 3 * mt * mt * t * c[0] + 3 * mt * t * t * c[2] + t * t * t
    val y = 3 * mt * mt * t * c[1] + 3 * mt * t * t * c[3] + t * t * t
    return x to y
}

/** Cubic-Bezier pressure curve, bisection 12 iterations (spec 8). */
fun applyPressureCurve(curve: DoubleArray, p: Double): Double {
    var lo = 0.0
    var hi = 1.0
    repeat(12) {
        val mid = (lo + hi) / 2
        if (bezier(curve, mid).first < p) lo = mid else hi = mid
    }
    return bezier(curve, (lo + hi) / 2).second
}
