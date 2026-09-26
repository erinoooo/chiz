package xyz.chiz.tablet.ui

import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val Bg = Color(0xFF0E0E12)
private val Surface = Color(0xFF17171D)
private val SurfaceHigh = Color(0xFF232329)
private val Amber = Color(0xFFE8C547)
private val OnAmber = Color(0xFF1A1503)
private val Teal = Color(0xFF7DD0C3)
private val Danger = Color(0xFFF44336)
private val Go = Color(0xFF4CAF50)

private val Scheme = darkColorScheme(
    background = Bg,
    surface = Surface,
    surfaceVariant = SurfaceHigh,
    primary = Amber,
    onPrimary = OnAmber,
    secondary = Teal,
    error = Danger,
)

/** Chiz dark theme: near-black surfaces, amber primary, teal secondary. */
@Composable
fun ChizTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = Scheme, typography = Typography(), content = content)
}
