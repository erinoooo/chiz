package xyz.chiz.tablet.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import xyz.chiz.tablet.R

/** The pen-nib mark, tinted with the brand amber. */
@Composable
fun LogoMark(size: Dp = 40.dp) {
    Image(
        painter = painterResource(R.drawable.ic_logo_mono),
        contentDescription = null,
        modifier = Modifier.size(size),
        colorFilter = ColorFilter.tint(ChizColors.Amber),
    )
}

/** App title row used at the top of the full-screen pages. */
@Composable
fun ChizHeader(subtitle: String) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        LogoMark(46.dp)
        Spacer(Modifier.width(10.dp))
        Column {
            Text("Chiz", style = MaterialTheme.typography.headlineMedium, color = ChizColors.Amber)
            Text(subtitle, style = MaterialTheme.typography.bodyMedium, color = ChizColors.TextDim)
        }
    }
}

/** Rounded surface block; the basic unit of every list screen. */
@Composable
fun SectionCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Card(
        modifier = modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.large,
        colors = CardDefaults.cardColors(containerColor = ChizColors.Surface),
        border = BorderStroke(1.dp, ChizColors.Border),
    ) {
        Column(Modifier.padding(18.dp), Arrangement.spacedBy(10.dp), content = content)
    }
}

/** Small uppercase caption above a group of settings. */
@Composable
fun SectionLabel(text: String) {
    Text(
        text.uppercase(),
        style = MaterialTheme.typography.labelMedium,
        color = ChizColors.TextDim,
        modifier = Modifier.padding(top = 10.dp, bottom = 2.dp, start = 4.dp),
    )
}

@Composable
fun StatusDot(color: Color, size: Dp = 10.dp) {
    Box(Modifier.size(size).clip(CircleShape).background(color))
}

@Composable
fun SettingSwitch(title: String, subtitle: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
        Column(Modifier.weight(1f).padding(end = 12.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            if (subtitle != null) {
                Text(subtitle, style = MaterialTheme.typography.bodyMedium, color = ChizColors.TextDim)
            }
        }
        Switch(checked, onChange)
    }
}

@Composable
fun SettingSlider(
    title: String,
    valueLabel: String,
    value: Float,
    onChange: (Float) -> Unit,
    range: ClosedFloatingPointRange<Float>,
) {
    Column(Modifier.fillMaxWidth()) {
        Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(valueLabel, style = MaterialTheme.typography.bodyLarge, color = ChizColors.Amber)
        }
        Slider(value, onChange, valueRange = range)
    }
}

/** Single-choice chip group. */
@Composable
fun ChipChoice(title: String, options: List<String>, selected: String, onSelect: (String) -> Unit) {
    Column(Modifier.fillMaxWidth(), Arrangement.spacedBy(8.dp)) {
        Text(title, style = MaterialTheme.typography.bodyLarge)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            options.forEach { o ->
                FilterChip(o == selected, { onSelect(o) }, { Text(o.replaceFirstChar { it.uppercase() }) })
            }
        }
    }
}
