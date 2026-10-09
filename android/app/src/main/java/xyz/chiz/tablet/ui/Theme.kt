package xyz.chiz.tablet.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Brand + status colors shared by every screen (same palette as the PC app). */
object ChizColors {
    val Bg = Color(0xFF0E0E12)
    val Surface = Color(0xFF17171D)
    val SurfaceHigh = Color(0xFF232329)
    val Border = Color(0xFF2C2C36)
    val Amber = Color(0xFFE8C547)
    val OnAmber = Color(0xFF1A1503)
    val Teal = Color(0xFF7DD0C3)
    val Text = Color(0xFFECECF1)
    val TextDim = Color(0xFF9A9AA8)
    val Go = Color(0xFF4CC37A)
    val Warn = Color(0xFFFFC107)
    val Danger = Color(0xFFF25C54)
}

private val Scheme = darkColorScheme(
    primary = ChizColors.Amber,
    onPrimary = ChizColors.OnAmber,
    primaryContainer = Color(0xFF3A3110),
    onPrimaryContainer = Color(0xFFFFE9A0),
    secondary = ChizColors.Teal,
    onSecondary = Color(0xFF00201C),
    secondaryContainer = Color(0xFF1F3B37),
    onSecondaryContainer = Color(0xFFB8F0E6),
    background = ChizColors.Bg,
    onBackground = ChizColors.Text,
    surface = ChizColors.Bg,
    onSurface = ChizColors.Text,
    surfaceVariant = ChizColors.SurfaceHigh,
    onSurfaceVariant = ChizColors.TextDim,
    surfaceContainerLowest = Color(0xFF0A0A0D),
    surfaceContainerLow = ChizColors.Surface,
    surfaceContainer = ChizColors.Surface,
    surfaceContainerHigh = ChizColors.SurfaceHigh,
    surfaceContainerHighest = Color(0xFF2E2E38),
    outline = Color(0xFF55555F),
    outlineVariant = ChizColors.Border,
    error = ChizColors.Danger,
    onError = Color.White,
)

private val Type = Typography().let { t ->
    t.copy(
        headlineMedium = t.headlineMedium.copy(fontWeight = FontWeight.Bold, fontSize = 30.sp, lineHeight = 38.sp),
        titleLarge = t.titleLarge.copy(fontWeight = FontWeight.SemiBold),
        titleMedium = t.titleMedium.copy(fontWeight = FontWeight.SemiBold),
        labelLarge = t.labelLarge.copy(fontWeight = FontWeight.SemiBold, letterSpacing = 0.2.sp),
        labelMedium = t.labelMedium.copy(letterSpacing = 0.8.sp),
        bodyMedium = t.bodyMedium.copy(lineHeight = 22.sp),
        bodyLarge = t.bodyLarge.copy(lineHeight = 26.sp),
    )
}

private val ChizShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(22.dp),
    extraLarge = RoundedCornerShape(30.dp),
)

/** Chiz dark theme: near-black surfaces, amber primary, teal secondary. */
@Composable
fun ChizTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = Scheme, typography = Type, shapes = ChizShapes, content = content)
}
