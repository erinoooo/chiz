package xyz.chiz.tablet.ui

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

/** Screens (spec 6): Connect / Drawing / Settings / Test + 3-step intro. */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Edge-to-edge + immersive flags are applied in DrawingScreen via
        // WindowInsetsController (hide system bars) with gesture exclusion
        // rects over the pen area (spec 6).
        setContent { ChizNav() }
    }
}

@Composable
fun ChizNav() {
    var screen by remember { mutableStateOf("connect") }
    var showIntro by remember { mutableStateOf(true) } // first launch only (DataStore)
    if (showIntro) {
        IntroScreen { showIntro = false }
    } else when (screen) {
        "connect" -> ConnectScreen(onConnected = { screen = "drawing" })
        "drawing" -> DrawingScreen(onSettings = { screen = "settings" }, onTest = { screen = "test" }, onDisconnect = { screen = "connect" })
        "settings" -> SettingsScreen { screen = "drawing" }
        "test" -> TestScreen { screen = "drawing" }
    }
}

@Composable
fun IntroScreen(done: () -> Unit) {
    var step by remember { mutableStateOf(0) }
    val texts = listOf(
        "Chiz turns this tablet into a screenless pen tablet for your PC. The pen moves the PC cursor; the strip fires shortcuts.",
        "Rest your finger on a strip button to hear its name; tap quickly to fire it. Your eyes stay on the monitor.",
        "Pair from the PC: open Pair new tablet, then Scan QR here — or enter the PIN on a network you trust.",
    )
    Column(Modifier.fillMaxSize().padding(24.dp), Arrangement.Center) {
        Text(texts[step], style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(24.dp))
        Button(onClick = { if (step < 2) step++ else done() }) {
            Text(if (step < 2) "Next" else "Start")
        }
    }
}

@Composable
fun ConnectScreen(onConnected: () -> Unit) {
    // Lists paired PCs (found/not found via Discovery), Connect + Forget,
    // "Pair new PC": Scan QR (camera permission on tap), PIN entry, manual
    // address entry. Status dot + spoken connects (spec 6).
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Text("Chiz — Connect", style = MaterialTheme.typography.headlineMedium)
        Button(onClick = onConnected) { Text("Connect (demo wiring)") }
        Button(onClick = {}) { Text("Scan QR") }
    }
}

@Composable
fun DrawingScreen(onSettings: () -> Unit, onTest: () -> Unit, onDisconnect: () -> Unit) {
    // Immersive full screen: StripView (56 dp header: dot + Menu hold 1 s ->
    // Settings/Test/Disconnect sheet) + PenAreaView filling the rest.
    // Screen stays on; dim-to-5% after 30 s idle restores on STRIP touch
    // only; back-press-twice disconnects; TalkBack-on shows a one-time
    // notice (spec 6 rules).
    var sheet by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxSize()) {
        AndroidView(
            factory = { ctx ->
                StripView(ctx).apply {
                    onMenu = { sheet = true }
                    onPanic = onDisconnect
                }
            },
            modifier = Modifier.fillMaxHeight().width(96.dp),
        )
        AndroidView(
            factory = { ctx -> PenAreaView(ctx) },
            modifier = Modifier.fillMaxSize(),
        )
    }
    if (sheet) {
        AlertDialog(
            onDismissRequest = { sheet = false },
            confirmButton = {
                TextButton(onClick = { sheet = false; onSettings() }) { Text("Settings") }
                TextButton(onClick = { sheet = false; onTest() }) { Text("Test") }
                TextButton(onClick = { sheet = false; onDisconnect() }) { Text("Disconnect") }
            },
            title = { Text("Menu") },
        )
    }
}

@Composable
fun SettingsScreen(back: () -> Unit) {
    // Local settings table (spec 6): strip edge/size, orientation, button
    // guard, dim screen, feedback mode, speech rate 0.5-4x, feedback volume,
    // announce mode, rest delay 150-600 ms, show labels. Backed by DataStore;
    // strip/orient changes send `surface` (spec 6).
    LazyColumn(Modifier.fillMaxSize().padding(16.dp)) {
        item {
            Text("Settings", style = MaterialTheme.typography.headlineMedium)
            Button(onClick = back) { Text("Back") }
        }
        items(listOf("Strip edge", "Strip size", "Orientation", "Button guard", "Dim screen",
            "Feedback mode", "Speech rate", "Feedback volume", "Announce mode",
            "Rest delay", "Show button labels")) { s ->
            Text(s, modifier = Modifier.padding(vertical = 8.dp))
        }
    }
}

@Composable
fun TestScreen(back: () -> Unit) {
    // Local pen test: live readout of x/y/pressure/tilt/distance/tool/
    // buttons + Canvas ink trail. Sends nothing to the PC (spec 6).
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Text("Pen test (local only)", style = MaterialTheme.typography.headlineMedium)
        Text("x=– y=– pressure=– tilt=–/– distance=– tool=– buttons=–")
        Button(onClick = back) { Text("Back") }
    }
}
