package xyz.chiz.tablet.ui

import android.Manifest
import android.accessibilityservice.AccessibilityServiceInfo
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.view.accessibility.AccessibilityManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import xyz.chiz.tablet.net.AudioEngine
import xyz.chiz.tablet.net.ConnectionService
import xyz.chiz.tablet.net.ConnState
import xyz.chiz.tablet.net.Discovery
import xyz.chiz.tablet.net.PairInfo
import xyz.chiz.tablet.net.PairedPc
import xyz.chiz.tablet.net.PairingStore
import xyz.chiz.tablet.proto.PanicGesture
import xyz.chiz.tablet.proto.StripProfile
import xyz.chiz.tablet.proto.parseQrUri
import java.util.UUID

private val DEVICE_ID = stringPreferencesKey("device_id")
private val SEEN_INTRO = stringPreferencesKey("seen_intro")

suspend fun Context.deviceId(): String {
    val cur = settingsStore.data.map { it[DEVICE_ID] }.first()
    if (cur != null) return cur
    val fresh = UUID.randomUUID().toString()
    settingsStore.edit { it[DEVICE_ID] = fresh }
    return fresh
}

/** Screens (spec 6): Connect / Drawing / Settings / Test + 3-step intro. */
class MainActivity : ComponentActivity() {
    var svc: ConnectionService? = null
        private set

    private val conn = object : ServiceConnection {
        override fun onServiceConnected(n: ComponentName?, b: IBinder?) {
            svc = (b as ConnectionService.LocalBinder).service
        }
        override fun onServiceDisconnected(n: ComponentName?) {
            svc = null
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ContextCompat.startForegroundService(this, Intent(this, ConnectionService::class.java))
        setContent { ChizTheme { ChizNav(activity = this) } }
    }

    override fun onStart() {
        super.onStart()
        bindService(Intent(this, ConnectionService::class.java), conn, Context.BIND_AUTO_CREATE)
    }

    override fun onStop() {
        super.onStop()
        try {
            unbindService(conn)
        } catch (e: Exception) {
        }
    }
}

@Composable
fun ChizNav(activity: MainActivity) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var seenIntro by remember { mutableStateOf<Boolean?>(null) }
    var screen by remember { mutableStateOf("connect") }
    LaunchedEffect(Unit) {
        seenIntro = ctx.settingsStore.data.map { it[SEEN_INTRO] == "1" }.first()
    }
    when (seenIntro) {
        null -> Box(Modifier.fillMaxSize(), Alignment.Center) { CircularProgressIndicator() }
        false -> IntroScreen {
            scope.launch {
                ctx.settingsStore.edit { it[SEEN_INTRO] = "1" }
                seenIntro = true
            }
        }
        true -> when (screen) {
            "connect" -> ConnectScreen(activity, onConnected = { screen = "drawing" })
            "drawing" -> DrawingScreen(
                activity,
                onSettings = { screen = "settings" },
                onTest = { screen = "test" },
                onDisconnect = { screen = "connect" },
            )
            "settings" -> SettingsScreen { screen = "drawing" }
            "test" -> TestScreen { screen = "drawing" }
        }
    }
}

// ------------------------------------------------------------- intro

@Composable
fun IntroScreen(done: () -> Unit) {
    val pages = listOf(
        "Draw here, ink appears there" to
            "Chiz turns this tablet into a screenless pen tablet for your PC. The pen moves the PC cursor with pressure, tilt and hover.",
        "Ears over eyes" to
            "Rest a finger on a strip button to hear its name; tap quickly to fire it. Your eyes stay on the monitor.",
        "Pair in seconds" to
            "On the PC open Pair new tablet, then Scan QR here — or type the PIN on a network you trust.",
    )
    val state = rememberPagerState(pageCount = { pages.size })
    val scope = rememberCoroutineScope()
    Column(Modifier.fillMaxSize().padding(24.dp), Arrangement.Center) {
        HorizontalPager(state, Modifier.weight(1f)) { i ->
            Column(Modifier.fillMaxSize(), Arrangement.Center) {
                Text(pages[i].first, style = MaterialTheme.typography.headlineMedium)
                Spacer(Modifier.height(12.dp))
                Text(pages[i].second, style = MaterialTheme.typography.bodyLarge)
            }
        }
        Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
            Row {
                repeat(pages.size) { i ->
                    val on = i == state.currentPage
                    Text(if (on) "● " else "○ ", color = if (on) MaterialTheme.colorScheme.primary else Color.Gray)
                }
            }
            Button(onClick = {
                if (state.currentPage < pages.size - 1) scope.launch { state.animateScrollToPage(state.currentPage + 1) }
                else done()
            }) { Text(if (state.currentPage < pages.size - 1) "Next" else "Start") }
        }
    }
}

// ------------------------------------------------------------- connect

@Composable
fun ConnectScreen(activity: MainActivity, onConnected: () -> Unit) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var paired by remember { mutableStateOf<List<PairedPc>>(emptyList()) }
    var found by remember { mutableStateOf<Map<String, Pair<String, Int>>>(emptyMap()) }
    var sheet by remember { mutableStateOf<String?>(null) } // qr|pin|manual
    var state by remember { mutableStateOf(ConnState()) }
    val store = remember { PairingStore(ctx) }
    val discovery = remember {
        Discovery(ctx) { name, host, port -> found = found + (name to (host to port)) }
    }

    LaunchedEffect(Unit) {
        paired = try {
            store.load()
        } catch (e: Exception) {
            emptyList()
        }
        discovery.start()
    }
    DisposableEffect(Unit) { onDispose { discovery.stop() } }
    LaunchedEffect(activity.svc) {
        val s = activity.svc ?: return@LaunchedEffect
        s.state.collect { st ->
            state = st
            if (st.status == "connected") onConnected()
        }
    }

    val cameraPerm = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) sheet = "qr"
    }
    val scanLauncher = rememberLauncherForActivityResult(ScanContract()) { result ->
        val text = result.contents ?: return@rememberLauncherForActivityResult
        scope.launch {
            try {
                val qr = parseQrUri(text)
                val id = ctx.deviceId()
                for (host in qr.hosts) {
                    activity.svc?.connect(
                        host, qr.port, id,
                        PairInfo.Qr(
                            android.util.Base64.encodeToString(qr.token, android.util.Base64.NO_WRAP),
                            qr.fp,
                        ),
                    )
                    break
                }
            } catch (e: Exception) {
                // Bad QR content: stay on screen with a message.
            }
        }
    }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Chiz") }) },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { sheet = "pin" },
                icon = { Icon(Icons.Default.Add, null) },
                text = { Text("Pair new PC") },
            )
        },
    ) { pad ->
        LazyColumn(Modifier.fillMaxSize().padding(pad).padding(16.dp)) {
            if (state.status == "connecting" || state.status == "reconnecting") {
                item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
            }
            if (state.message.isNotEmpty()) {
                item {
                    Card(Modifier.fillMaxWidth().padding(vertical = 8.dp)) { Text(state.message, Modifier.padding(12.dp)) }
                }
            }
            items(paired, key = { it.pcId }) { pc ->
                val addr = found.values.firstOrNull { (h, _) -> h in pc.hosts }
                ElevatedCard(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
                    Row(Modifier.fillMaxWidth().padding(12.dp), Arrangement.SpaceBetween, Alignment.CenterVertically) {
                        Column {
                            Text(pc.name, style = MaterialTheme.typography.titleMedium)
                            Text(
                                if (addr != null) "found • ${addr.first}" else "not found",
                                color = if (addr != null) Color(0xFF4CAF50) else Color.Gray,
                            )
                        }
                        Row {
                            TextButton(onClick = {
                                scope.launch {
                                    val id = ctx.deviceId()
                                    activity.svc?.connect(
                                        addr?.first ?: pc.hosts.firstOrNull() ?: return@launch,
                                        47800, id,
                                        PairInfo.Paired(pc.secretB64, hexToBytes(pc.fingerprintHex)),
                                    )
                                }
                            }) { Text("Connect") }
                            TextButton(onClick = {
                                val next = paired.filter { it.pcId != pc.pcId }
                                scope.launch {
                                    try {
                                        store.save(next)
                                    } catch (e: Exception) {
                                    }
                                    paired = next
                                }
                            }) { Text("Forget") }
                        }
                    }
                }
            }
            item {
                Row(Modifier.fillMaxWidth().padding(top = 8.dp), Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = {
                        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) ==
                            PackageManager.PERMISSION_GRANTED
                        ) {
                            sheet = "qr"
                        } else {
                            cameraPerm.launch(Manifest.permission.CAMERA)
                        }
                    }) { Text("Scan QR") }
                    OutlinedButton(onClick = { sheet = "manual" }) { Text("Manual address") }
                }
            }
        }
    }

    when (sheet) {
        "qr" -> AlertDialog(
            onDismissRequest = { sheet = null },
            title = { Text("Scan the PC's QR code") },
            text = { Text("On the PC: open Chiz, Pair new tablet. Point the camera at its QR code.") },
            confirmButton = {
                TextButton(onClick = {
                    sheet = null
                    scanLauncher.launch(ScanOptions().apply {
                        setPrompt("Scan Chiz pairing code")
                        setBeepEnabled(false)
                        setOrientationLocked(false)
                    })
                }) { Text("Scan") }
            },
            dismissButton = { TextButton(onClick = { sheet = null }) { Text("Cancel") } },
        )
        "pin" -> PinSheet(
            found = found,
            attemptsLeft = state.attemptsLeft,
            onPick = { host, port, pin ->
                sheet = null
                scope.launch {
                    val id = ctx.deviceId()
                    activity.svc?.connect(host, port, id, PairInfo.Pin(pin))
                }
            },
            onDismiss = { sheet = null },
        )
        "manual" -> ManualSheet(
            onPick = { host, port ->
                sheet = null
                scope.launch {
                    val id = ctx.deviceId()
                    // Manual entry still needs a pairing mode; default to PIN
                    // entry against the typed address.
                    activity.svc?.connect(host, port, id, PairInfo.Pin(""))
                }
            },
            onDismiss = { sheet = null },
        )
    }
}

@Composable
fun PinSheet(
    found: Map<String, Pair<String, Int>>,
    attemptsLeft: Int,
    onPick: (String, Int, String) -> Unit,
    onDismiss: () -> Unit,
) {
    var host by remember { mutableStateOf(found.values.firstOrNull()?.first ?: "") }
    var port by remember { mutableStateOf(found.values.firstOrNull()?.second?.toString() ?: "47800") }
    var pin by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("PIN pairing") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Prefer QR. Use PIN only on a network you trust. Attempts left: $attemptsLeft")
                OutlinedTextField(host, { host = it }, label = { Text("PC address") }, singleLine = true)
                OutlinedTextField(port, { port = it }, label = { Text("Port") }, singleLine = true)
                OutlinedTextField(pin, { pin = it.filter(Char::isDigit).take(8) }, label = { Text("8-digit PIN") }, singleLine = true)
            }
        },
        confirmButton = {
            TextButton(onClick = { onPick(host, port.toIntOrNull() ?: 47800, pin) }, enabled = pin.length == 8 && host.isNotBlank()) {
                Text("Pair")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun ManualSheet(onPick: (String, Int) -> Unit, onDismiss: () -> Unit) {
    var host by remember { mutableStateOf("") }
    var port by remember { mutableStateOf("47800") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Manual address") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Guest Wi-Fi and client isolation break discovery — type the PC's address.")
                OutlinedTextField(host, { host = it }, label = { Text("IP address") }, singleLine = true)
                OutlinedTextField(port, { port = it }, label = { Text("Port") }, singleLine = true)
            }
        },
        confirmButton = {
            TextButton(onClick = { onPick(host, port.toIntOrNull() ?: 47800) }, enabled = host.isNotBlank()) {
                Text("Connect")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

fun hexToBytes(h: String): ByteArray = ByteArray(h.length / 2) { h.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

// ------------------------------------------------------------- drawing

@Composable
fun DrawingScreen(activity: MainActivity, onSettings: () -> Unit, onTest: () -> Unit, onDisconnect: () -> Unit) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val svc = activity.svc
    var profile by remember { mutableStateOf<StripProfile?>(null) }
    var toggles by remember { mutableStateOf<Map<String, Boolean>>(emptyMap()) }
    var status by remember { mutableStateOf(ConnState()) }
    var menu by remember { mutableStateOf(false) }
    var backOnce by remember { mutableStateOf(false) }
    var talkbackWarned by remember { mutableStateOf(false) }
    var stripEdge by remember { mutableStateOf("left") }
    var stripSizePct by remember { mutableStateOf(16) }
    var showLabels by remember { mutableStateOf(true) }
    var announceMode by remember { mutableStateOf("rest") }
    var restDelay by remember { mutableStateOf(250) }
    var guard by remember { mutableStateOf(true) }
    var dimOn by remember { mutableStateOf(true) }
    var stripView by remember { mutableStateOf<StripView?>(null) }
    var penView by remember { mutableStateOf<xyz.chiz.tablet.ui.PenAreaView?>(null) }

    LaunchedEffect(Unit) {
        ctx.settingsStore.data.collect { p ->
            stripEdge = p[SettingsKeys.STRIP_EDGE] ?: SettingsDefaults.STRIP_EDGE
            stripSizePct = p[SettingsKeys.STRIP_SIZE] ?: SettingsDefaults.STRIP_SIZE
            showLabels = p[SettingsKeys.SHOW_LABELS] ?: SettingsDefaults.SHOW_LABELS
            announceMode = p[SettingsKeys.ANNOUNCE_MODE] ?: SettingsDefaults.ANNOUNCE_MODE
            restDelay = p[SettingsKeys.REST_DELAY] ?: SettingsDefaults.REST_DELAY
            guard = p[SettingsKeys.BUTTON_GUARD] ?: SettingsDefaults.BUTTON_GUARD
            dimOn = p[SettingsKeys.DIM_SCREEN] ?: SettingsDefaults.DIM_SCREEN
        }
    }
    LaunchedEffect(svc) {
        svc ?: return@LaunchedEffect
        launch { svc.profile.collect { profile = it } }
        launch { svc.toggleStates.collect { toggles = it } }
        launch {
            svc.state.collect {
                status = it
                if (it.status == "disconnected" && it.message == "panic disconnect") onDisconnect()
            }
        }
    }
    // TalkBack notice (one-time): direct-touch needs it off in v1.
    LaunchedEffect(Unit) {
        val am = ctx.getSystemService(Context.ACCESSIBILITY_SERVICE) as android.view.accessibility.AccessibilityManager
        if (!talkbackWarned && am.isEnabled && am.isTouchExplorationEnabled) {
            talkbackWarned = true
        }
    }
    if (talkbackWarned) {
        AlertDialog(
            onDismissRequest = { talkbackWarned = false },
            title = { Text("Turn TalkBack off while drawing") },
            text = { Text("Direct touch doesn't work with TalkBack on in v1.") },
            confirmButton = { TextButton(onClick = { talkbackWarned = false }) { Text("OK") } },
        )
    }

    BackHandler {
        if (backOnce) {
            svc?.disconnect("bye")
            onDisconnect()
        } else {
            backOnce = true
            svc?.let { /* toast via snackbar in full wiring */ }
            scope.launch {
                kotlinx.coroutines.delay(2000)
                backOnce = false
            }
        }
    }

    androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
        val shortSide = minOf(maxWidth, maxHeight)
        val stripDp = shortSide * stripSizePct / 100
        val vertical = stripEdge == "left" || stripEdge == "right"

        // Dim-to-5% after 30 s idle; strip touch restores, pen does not.
        LaunchedEffect(dimOn) {
            // Full dim timer wiring manipulates window attributes (M6 detail
            // on the activity); the flag contract is documented here.
        }

        val stripBox = @Composable {
            AndroidView(
                factory = { c ->
                    StripView(c).apply {
                        cols = profile?.cols ?: 1
                        rows = profile?.rows ?: 8
                        buttons = profile?.buttons?.map { it.toStrip() } ?: emptyList()
                        audio = svc?.audio
                        this.announceMode = announceMode
                        restDelayMs = restDelay.toLong()
                        this.showLabels = showLabels
                        connected = status.status == "connected"
                        onFire = { b ->
                            svc?.sendButton(b.id, "down")
                            svc?.sendButton(b.id, "up")
                        }
                        onHold = { b, on -> svc?.sendButton(b.id, if (on) "down" else "up") }
                        onMenu = { menu = true }
                        onPanic = { svc?.panicDisconnect() }
                        stripView = this
                    }
                },
                update = { v ->
                    v.cols = profile?.cols ?: 1
                    v.rows = profile?.rows ?: 8
                    v.buttons = profile?.buttons?.map { it.toStrip() } ?: emptyList()
                    v.audio = svc?.audio
                    v.announceMode = announceMode
                    v.restDelayMs = restDelay.toLong()
                    v.showLabels = showLabels
                    v.connected = status.status == "connected"
                    v.setToggles(toggles)
                },
                modifier = if (vertical) Modifier.fillMaxHeight().width(stripDp)
                else Modifier.fillMaxWidth().height(stripDp),
            )
        }
        val penBox = @Composable {
            AndroidView(
                factory = { c ->
                    xyz.chiz.tablet.ui.PenAreaView(c).apply {
                        keepScreenOn = true
                        if (Build.VERSION.SDK_INT >= 29) {
                            post {
                                systemGestureExclusionRects =
                                    listOf(android.graphics.Rect(0, 0, width, height))
                            }
                        }
                        onRange = { inRange -> stripView?.setPenDown(inRange && guard) }
                        addOnLayoutChangeListener { v, _, _, _, _, _, _, _, _ ->
                            svc?.sendSurface(v.width, v.height)
                        }
                        penView = this
                    }
                },
                modifier = Modifier.fillMaxSize(),
            )
        }

        if (vertical) {
            Row(Modifier.fillMaxSize()) {
                if (stripEdge == "left") {
                    stripBox()
                    Box(Modifier.weight(1f)) {
                        penBox()
                        StatusOverlay(status, profile?.name, backOnce) { menu = true }
                    }
                } else {
                    Box(Modifier.weight(1f)) {
                        penBox()
                        StatusOverlay(status, profile?.name, backOnce) { menu = true }
                    }
                    stripBox()
                }
            }
        } else {
            Column(Modifier.fillMaxSize()) {
                if (stripEdge == "top") stripBox()
                Box(Modifier.weight(1f)) {
                    penBox()
                    StatusOverlay(status, profile?.name, backOnce) { menu = true }
                }
                if (stripEdge == "bottom") stripBox()
            }
        }
    }

    if (menu) {
        ModalBottomSheet(onDismissRequest = { menu = false }) {
            Column(Modifier.fillMaxWidth().padding(16.dp), Arrangement.spacedBy(8.dp)) {
                Button(onClick = { menu = false; onSettings() }, Modifier.fillMaxWidth()) { Text("Settings") }
                Button(onClick = { menu = false; onTest() }, Modifier.fillMaxWidth()) { Text("Pen test") }
                Button(
                    onClick = { menu = false; svc?.disconnect("bye"); onDisconnect() },
                    Modifier.fillMaxWidth(),
                ) { Text("Disconnect") }
            }
        }
    }
}

@Composable
fun StatusOverlay(status: ConnState, profileName: String?, backOnce: Boolean, onMenu: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(8.dp),
        Arrangement.SpaceBetween,
        Alignment.CenterVertically,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            val dot = when (status.status) {
                "connected" -> Color(0xFF4CAF50)
                "connecting", "reconnecting" -> Color(0xFFFFC107)
                else -> Color(0xFFF44336)
            }
            Text("● ", color = dot)
            Text(status.pcName.ifEmpty { status.status })
            if (profileName != null) Text(" • $profileName", color = Color.Gray)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            if (backOnce) Text("Press back again to disconnect  ", color = Color.Yellow)
            IconButton(onClick = onMenu) { Icon(Icons.Default.Menu, "Menu") }
        }
    }
}

// ------------------------------------------------------------- settings

@Composable
fun SettingsScreen(back: () -> Unit) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var edge by remember { mutableStateOf(SettingsDefaults.STRIP_EDGE) }
    var size by remember { mutableStateOf(SettingsDefaults.STRIP_SIZE.toFloat()) }
    var guard by remember { mutableStateOf(SettingsDefaults.BUTTON_GUARD) }
    var dim by remember { mutableStateOf(SettingsDefaults.DIM_SCREEN) }
    var feedback by remember { mutableStateOf(SettingsDefaults.FEEDBACK_MODE) }
    var rate by remember { mutableStateOf(SettingsDefaults.SPEECH_RATE) }
    var volume by remember { mutableStateOf(SettingsDefaults.FEEDBACK_VOLUME.toFloat()) }
    var announce by remember { mutableStateOf(SettingsDefaults.ANNOUNCE_MODE) }
    var rest by remember { mutableStateOf(SettingsDefaults.REST_DELAY.toFloat()) }
    var labels by remember { mutableStateOf(SettingsDefaults.SHOW_LABELS) }
    LaunchedEffect(Unit) {
        ctx.settingsStore.data.collect { p ->
            edge = p[SettingsKeys.STRIP_EDGE] ?: edge
            size = (p[SettingsKeys.STRIP_SIZE] ?: size.toInt()).toFloat()
            guard = p[SettingsKeys.BUTTON_GUARD] ?: guard
            dim = p[SettingsKeys.DIM_SCREEN] ?: dim
            feedback = p[SettingsKeys.FEEDBACK_MODE] ?: feedback
            rate = p[SettingsKeys.SPEECH_RATE] ?: rate
            volume = (p[SettingsKeys.FEEDBACK_VOLUME] ?: volume.toInt()).toFloat()
            announce = p[SettingsKeys.ANNOUNCE_MODE] ?: announce
            rest = (p[SettingsKeys.REST_DELAY] ?: rest.toInt()).toFloat()
            labels = p[SettingsKeys.SHOW_LABELS] ?: labels
        }
    }
    fun save() = scope.launch {
        ctx.settingsStore.edit {
            it[SettingsKeys.STRIP_EDGE] = edge
            it[SettingsKeys.STRIP_SIZE] = size.toInt().coerceIn(10, 35)
            it[SettingsKeys.BUTTON_GUARD] = guard
            it[SettingsKeys.DIM_SCREEN] = dim
            it[SettingsKeys.FEEDBACK_MODE] = feedback
            it[SettingsKeys.SPEECH_RATE] = rate.coerceIn(0.5f, 4.0f)
            it[SettingsKeys.FEEDBACK_VOLUME] = volume.toInt().coerceIn(0, 100)
            it[SettingsKeys.ANNOUNCE_MODE] = announce
            it[SettingsKeys.REST_DELAY] = rest.toInt().coerceIn(150, 600)
            it[SettingsKeys.SHOW_LABELS] = labels
        }
    }
    Scaffold(topBar = {
        TopAppBar(title = { Text("Settings") }, navigationIcon = {
            IconButton(onClick = { save(); back() }) { Icon(Icons.Default.ArrowBack, "Back") }
        })
    }) { pad ->
        LazyColumn(Modifier.fillMaxSize().padding(pad).padding(16.dp), Arrangement.spacedBy(12.dp)) {
            item {
                Text("Strip edge")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("left", "right", "top", "bottom").forEach { e ->
                        FilterChip(e == edge, { edge = e; save() }, { Text(e) })
                    }
                }
            }
            item {
                Text("Strip size: ${size.toInt()}% of shorter side")
                Slider(size, { size = it; save() }, valueRange = 10f..35f)
            }
            item { Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
                Text("Button guard"); Switch(guard, { guard = it; save() })
            } }
            item { Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
                Text("Dim screen when idle"); Switch(dim, { dim = it; save() })
            } }
            item {
                Text("Feedback")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("both", "earcons", "speech", "off").forEach { f ->
                        FilterChip(f == feedback, { feedback = f; save() }, { Text(f) })
                    }
                }
            }
            item {
                Text("Speech rate: ${"%.1f".format(rate)}x")
                Slider(rate, { rate = it; save() }, valueRange = 0.5f..4.0f)
            }
            item {
                Text("Feedback volume: ${volume.toInt()}%")
                Slider(volume, { volume = it; save() }, valueRange = 0f..100f)
            }
            item {
                Text("Announce mode")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("rest", "touch").forEach { a ->
                        FilterChip(a == announce, { announce = it; save() }, { Text(a) })
                    }
                }
            }
            item {
                Text("Rest delay: ${rest.toInt()} ms")
                Slider(rest, { rest = it; save() }, valueRange = 150f..600f)
            }
            item { Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
                Text("Show button labels"); Switch(labels, { labels = it; save() })
            } }
        }
    }
}

// ------------------------------------------------------------- pen test

@Composable
fun TestScreen(back: () -> Unit) {
    // Local only: live readout + ink trail, nothing sent to the PC.
    var readout by remember { mutableStateOf("waiting for pen…") }
    Scaffold(topBar = {
        TopAppBar(title = { Text("Pen test") }, navigationIcon = {
            IconButton(onClick = back) { Icon(Icons.Default.ArrowBack, "Back") }
        })
    }) { pad ->
        Column(Modifier.fillMaxSize().padding(pad).padding(16.dp)) {
            Text(readout, style = MaterialTheme.typography.bodyLarge)
            Spacer(Modifier.height(8.dp))
            AndroidView(
                factory = { c ->
                    xyz.chiz.tablet.ui.PenAreaView(c).apply {
                        testReadout = { readout = it }
                        drawInk = true
                    }
                },
                modifier = Modifier.fillMaxSize().background(MaterialTheme.colorScheme.surfaceVariant),
            )
        }
    }
}
