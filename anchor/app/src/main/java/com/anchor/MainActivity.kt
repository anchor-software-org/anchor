package com.anchor

import android.app.Activity
import android.content.Context
import android.content.pm.ActivityInfo
import android.graphics.Matrix
import android.graphics.RectF
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Create
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.HelpOutline
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.CameraAlt
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.FlipCameraAndroid
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import androidx.compose.material.icons.filled.Sms
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.viewinterop.AndroidView
import android.view.Surface as AndroidSurface
import android.view.TextureView
import com.anchor.R
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DrawerState
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.TextButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.foundation.Image
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.viewmodel.compose.viewModel
import com.anchor.data.ConnectionState
import com.anchor.data.ConnectionStatus
import com.anchor.data.PairedDeviceDisplay
import com.anchor.data.PairingState
import com.anchor.presentation.MainViewModel
import com.anchor.ui.Haptics
import com.anchor.ui.TouchpadSurface
import com.anchor.ui.VideoSurface
import com.anchor.ui.rememberHaptics
import com.anchor.ui.theme.*
import android.Manifest
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.anchor.plugin.AnchorNotificationListener
import com.anchor.plugin.InputPlugin
import com.anchor.plugin.MediaPlaybackState
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Checkbox
import androidx.compose.runtime.produceState
import androidx.core.graphics.drawable.toBitmap
import kotlin.math.abs

// ─── Navigation ─────────────────────────────────────────────────────────

enum class Screen { Main, Sideboat, Settings, Notifications, NotificationApps, Sms, RemoteInput, Clipboard, Camera, Media, Commands, Files }

private const val TIMING_TAG = "anchor.timing"
private const val STREAM_TOUCH_MOTION_INTERVAL_NS = 33_000_000L

// ─── Activity ───────────────────────────────────────────────────────────

class MainActivity : ComponentActivity() {
    /** Screen requested by an inbound intent (e.g. media notification tap). */
    private val pendingScreen = mutableStateOf<Screen?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        pendingScreen.value = screenFromIntent(intent)
        setContent { AnchorTheme { AnchorApp(pendingScreen) } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        screenFromIntent(intent)?.let { pendingScreen.value = it }
    }

    private fun screenFromIntent(intent: Intent?): Screen? = when (intent?.getStringExtra("open_screen")) {
        "media" -> Screen.Media
        else -> null
    }

}

// ─── Immersive mode (keep) ──────────────────────────────────────────────

@Composable
fun ImmersiveMode(enabled: Boolean) {
    val view = LocalView.current
    DisposableEffect(enabled) {
        val window = (view.context as? ComponentActivity)?.window ?: return@DisposableEffect onDispose {}
        val controller = WindowCompat.getInsetsController(window, view)
        if (enabled) {
            controller.hide(WindowInsetsCompat.Type.systemBars())
            controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        } else {
            controller.show(WindowInsetsCompat.Type.systemBars())
            controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_DEFAULT
        }
        onDispose {
            controller.show(WindowInsetsCompat.Type.systemBars())
            controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_DEFAULT
        }
    }
}

// ─── Stream touch input ─────────────────────────────────────────────────

/**
 * Touch handling for the sideboat video surface, in one of two modes:
 *
 * * **absolute** (`touchpadMode = false`): the finger position maps directly to
 *   the desktop cursor (normalized by the view size). A short tap clicks; a
 *   long-press-then-drag holds the button down while moving.
 * * **touchpad** (`touchpadMode = true`): like a laptop touchpad — dragging
 *   moves the cursor *relatively* (finger position is not the cursor position);
 *   a tap clicks.
 *
 * [haptics] fires a click on each discrete button press.
 */
private fun Modifier.streamTouchInput(
    inputPlugin: InputPlugin,
    touchpadMode: Boolean,
    sensitivity: Float,
    haptics: Haptics
): Modifier = pointerInput(touchpadMode) {
    if (touchpadMode) {
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false)
            var prevX = down.position.x
            var prevY = down.position.y
            var moved = false
            var lastSendTime = 0L
            val tapThreshold = 12f

            do {
                val event = awaitPointerEvent()
                val pointer = event.changes.firstOrNull() ?: break
                val dx = pointer.position.x - prevX
                val dy = pointer.position.y - prevY
                if (abs(dx) > 1f || abs(dy) > 1f) {
                    moved = true
                    val now = System.nanoTime()
                    if (now - lastSendTime >= STREAM_TOUCH_MOTION_INTERVAL_NS) {
                        inputPlugin.sendMotion(dx * sensitivity, dy * sensitivity)
                        lastSendTime = now
                        prevX = pointer.position.x
                        prevY = pointer.position.y
                    }
                }
                pointer.consume()
            } while (event.changes.any { it.pressed })

            val travelled = abs(down.position.x - prevX) > tapThreshold ||
                abs(down.position.y - prevY) > tapThreshold
            if (!moved || !travelled) {
                haptics.click()
                inputPlugin.sendButton(pressed = true)
                inputPlugin.sendButton(pressed = false)
            }
        }
    } else {
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false)
            val viewW = size.width.toFloat()
            val viewH = size.height.toFloat()

            val downTime = System.currentTimeMillis()
            val downElapsedMs = SystemClock.elapsedRealtime()
            var dragging = false
            var buttonDown = false
            var lastSendTime: Long
            var moveSamples = 0

            val dx = (down.position.x / viewW).coerceIn(0f, 1f)
            val dy = (down.position.y / viewH).coerceIn(0f, 1f)
            Log.d(
                TIMING_TAG,
                "[input] gesture_down x=${"%.3f".format(dx)} y=${"%.3f".format(dy)} view=${viewW.toInt()}x${viewH.toInt()}"
            )
            inputPlugin.sendMotionAbsolute(dx, dy)
            lastSendTime = System.nanoTime()
            var lastSentX = dx
            var lastSentY = dy
            var latestX = dx
            var latestY = dy

            try {
                do {
                    val event = awaitPointerEvent()
                    val pointer = event.changes.firstOrNull() ?: break
                    latestX = (pointer.position.x / viewW).coerceIn(0f, 1f)
                    latestY = (pointer.position.y / viewH).coerceIn(0f, 1f)

                    val now = System.nanoTime()
                    if (now - lastSendTime >= STREAM_TOUCH_MOTION_INTERVAL_NS) {
                        moveSamples++
                        val sendGapMs = (now - lastSendTime) / 1_000_000.0
                        if (moveSamples % 30 == 0 || sendGapMs > 40.0) {
                            Log.d(
                                TIMING_TAG,
                                "[input] gesture_move count=$moveSamples since_down_ms=${SystemClock.elapsedRealtime() - downElapsedMs} send_gap_ms=${"%.1f".format(sendGapMs)} x=${"%.3f".format(latestX)} y=${"%.3f".format(latestY)}"
                            )
                        }
                        inputPlugin.sendMotionAbsolute(latestX, latestY)
                        lastSentX = latestX
                        lastSentY = latestY
                        lastSendTime = now
                    }

                    if (!dragging && System.currentTimeMillis() - downTime > 300) {
                        dragging = true
                        Log.d(
                            TIMING_TAG,
                            "[input] drag_start after_ms=${SystemClock.elapsedRealtime() - downElapsedMs}"
                        )
                        haptics.click()
                        buttonDown = true
                        inputPlugin.sendButton(pressed = true)
                    }

                    pointer.consume()
                } while (event.changes.any { it.pressed })

                if (dragging) {
                    if (abs(latestX - lastSentX) > 0.0001f || abs(latestY - lastSentY) > 0.0001f) {
                        Log.d(
                            TIMING_TAG,
                            "[input] drag_end_flush x=${"%.3f".format(latestX)} y=${"%.3f".format(latestY)}"
                        )
                        inputPlugin.sendMotionAbsolute(latestX, latestY)
                    }
                    Log.d(
                        TIMING_TAG,
                        "[input] drag_end duration_ms=${SystemClock.elapsedRealtime() - downElapsedMs} moves=$moveSamples"
                    )
                    inputPlugin.sendButton(pressed = false)
                    buttonDown = false
                } else {
                    // A tap is any gesture that did not cross the long-press
                    // threshold. Do not leave a duration gap where a normal
                    // tap emits no click at all.
                    Log.d(
                        TIMING_TAG,
                        "[input] tap duration_ms=${SystemClock.elapsedRealtime() - downElapsedMs}"
                    )
                    haptics.click()
                    inputPlugin.sendButton(pressed = true)
                    inputPlugin.sendButton(pressed = false)
                }
            } finally {
                // Pointer input can be cancelled when the surface loses focus,
                // is replaced, or the app backgrounds. Never leave the host
                // compositor with a logically pressed button.
                if (buttonDown) {
                    Log.w("anchor.input", "[input] gesture cancelled; releasing pointer button")
                    inputPlugin.sendButton(pressed = false)
                    buttonDown = false
                }
            }
        }
    }
}

// ─── Root ───────────────────────────────────────────────────────────────

@Composable
fun AnchorApp(pendingScreen: MutableState<Screen?> = mutableStateOf(null)) {
    val viewModel = viewModel<MainViewModel>()
    var currentScreen by remember { mutableStateOf(Screen.Main) }

    // Route deep-link intents (e.g. media notification tap) to the requested screen.
    LaunchedEffect(pendingScreen.value) {
        pendingScreen.value?.let {
            currentScreen = it
            pendingScreen.value = null
        }
    }
    var isFullscreen by remember { mutableStateOf(false) }
    val drawerState = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()

    // Back button handling
    BackHandler(enabled = isFullscreen || drawerState.isOpen || currentScreen != Screen.Main) {
        when {
            isFullscreen -> isFullscreen = false
            drawerState.isOpen -> scope.launch { drawerState.close() }
            currentScreen != Screen.Main -> currentScreen = Screen.Main
        }
    }

    ImmersiveMode(enabled = isFullscreen)

    val connectionState by viewModel.connectionState.collectAsState()
    val touchInputEnabled by viewModel.touchInputOnStream.collectAsState()
    val sideboatTouchpadMode by viewModel.sideboatTouchpadMode.collectAsState()
    val streamSensitivity by viewModel.touchpadSensitivity.collectAsState()
    val hapticsEnabled by viewModel.hapticsEnabled.collectAsState()
    val streamHaptics = rememberHaptics(hapticsEnabled)

    // Fullscreen: video with optional touch input overlay
    if (isFullscreen) {
        Box(Modifier.fillMaxSize()) {
            VideoSurface(
                videoPlugin = viewModel.videoPlugin,
                modifier = if (touchInputEnabled) {
                    Modifier.fillMaxSize().streamTouchInput(
                        inputPlugin = viewModel.inputPlugin,
                        touchpadMode = sideboatTouchpadMode,
                        sensitivity = streamSensitivity,
                        haptics = streamHaptics
                    )
                } else {
                    Modifier.fillMaxSize()
                },
                isFullscreen = true,
                onToggleFullscreen = if (touchInputEnabled) { {} } else { { isFullscreen = false } }
            )

            // Exit button — always visible in fullscreen
            FilledIconButton(
                onClick = { isFullscreen = false },
                shape = RoundedCornerShape(6.dp),
                colors = IconButtonDefaults.filledIconButtonColors(
                    containerColor = CharcoalBlack.copy(alpha = 0.72f),
                    contentColor = PureWhite
                ),
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(16.dp)
                    .size(38.dp)
            ) {
                Icon(Icons.Default.Close, contentDescription = "Exit fullscreen", modifier = Modifier.size(20.dp))
            }
        }
    } else {
        ModalNavigationDrawer(
            drawerState = drawerState,
            drawerContent = {
                ModalDrawerSheet(
                    drawerContainerColor = CharcoalBlack
                ) {
                    DrawerContent(
                        viewModel = viewModel,
                        currentScreen = currentScreen,
                        drawerState = drawerState,
                        onNavigate = { screen ->
                            currentScreen = screen
                            scope.launch { drawerState.close() }
                        }
                    )
                }
            }
        ) {
            when (currentScreen) {
                Screen.Main -> MainContent(
                    viewModel = viewModel,
                    onMenuClick = { scope.launch { drawerState.open() } },
                    onSideboat = { currentScreen = Screen.Sideboat }
                )
                Screen.Sideboat -> SideboatScreen(
                    viewModel = viewModel,
                    onMenuClick = { scope.launch { drawerState.open() } },
                    onFullscreen = { isFullscreen = true }
                )
                Screen.Settings -> SettingsScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } },
                    onOpenIgnoredApps = { currentScreen = Screen.NotificationApps }
                )
                Screen.NotificationApps -> NotificationAppsScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Settings },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.Notifications -> NotificationsScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.Sms -> SmsAccessScreen(
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.RemoteInput -> RemoteInputScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.Clipboard -> com.anchor.ui.ClipboardScreen(
                    clipboardPlugin = viewModel.clipboardPlugin,
                    isConnected = connectionState.status == ConnectionStatus.CONNECTED,
                    onBack = { currentScreen = Screen.Main }
                )
                Screen.Files -> com.anchor.ui.FilesScreen(
                    fileTransferPlugin = viewModel.fileTransferPlugin,
                    onBack = { currentScreen = Screen.Main }
                )
                Screen.Camera -> CameraScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.Media -> MediaScreen(
                    viewModel = viewModel,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                Screen.Commands -> com.anchor.ui.CommandsScreen(
                    commandsPlugin = viewModel.commandsPlugin,
                    isConnected = connectionState.status == ConnectionStatus.CONNECTED,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
                else -> PlaceholderScreen(
                    title = currentScreen.name,
                    onBack = { currentScreen = Screen.Main },
                    onMenuClick = { scope.launch { drawerState.open() } }
                )
            }
        }
    }

    // Pairing dialog overlay
    val currentPairing by viewModel.pairingState.collectAsState()
    val pairing = currentPairing
    if (pairing is PairingState.Requested) {
        PairingDialog(
            pairingState = pairing,
            onAccept = { viewModel.respondToPairing(true) },
            onReject = { viewModel.respondToPairing(false) }
        )
    }
}

// ─── Drawer ─────────────────────────────────────────────────────────────

@Composable
fun DrawerContent(viewModel: MainViewModel, currentScreen: Screen, drawerState: DrawerState, onNavigate: (Screen) -> Unit) {
    val connectionState by viewModel.connectionState.collectAsState()
    DrawerContentLayout(
        deviceLabel = android.os.Build.MODEL,
        isConnected = connectionState.status == ConnectionStatus.CONNECTED,
        host = connectionState.host,
        drawerOpen = drawerState.isOpen,
        onNavigate = onNavigate
    )
}

@Composable
private fun DrawerContentLayout(
    deviceLabel: String,
    isConnected: Boolean,
    host: String,
    drawerOpen: Boolean,
    onNavigate: (Screen) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
    ) {
        // Header with gradient + anchor icon
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(Brush.verticalGradient(listOf(DeepOcean, CharcoalBlack)))
                .padding(start = 20.dp, end = 20.dp, top = 52.dp, bottom = 20.dp)
        ) {
            Column {
                Text("Anchor", fontSize = 24.sp, fontWeight = FontWeight.Bold, color = PureWhite)
                Text(
                    deviceLabel,
                    fontSize = 12.sp,
                    color = AnchorGray
                )
            }
            val targetAngle = if (drawerOpen) 0f else 25f
            val animatedAngle by androidx.compose.animation.core.animateFloatAsState(
                targetValue = targetAngle,
                animationSpec = spring(
                    dampingRatio = 0.35f,
                    stiffness = 15f
                ),
                label = "anchorSwing"
            )

            Image(
                painter = painterResource(id = R.drawable.anchor_logo),
                contentDescription = null,
                alpha = 0.15f,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .offset(y = (-10).dp)
                    .size(80.dp)
                    .graphicsLayer {
                        transformOrigin = TransformOrigin(0.25f, 0.13f)
                        rotationZ = animatedAngle
                    }
            )
        }

        if (isConnected) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(12.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(DarkGray.copy(alpha = 0.5f))
                    .clickable { onNavigate(Screen.Main) }
                    .padding(14.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                Box(
                    modifier = Modifier
                        .size(38.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(DeepOcean),
                    contentAlignment = Alignment.Center
                ) {
                    Icon(
                        Icons.Default.Computer,
                        contentDescription = null,
                        tint = PureWhite,
                        modifier = Modifier.size(20.dp)
                    )
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(host.ifEmpty { "Desktop" }, fontSize = 14.sp, fontWeight = FontWeight.Medium, color = OffWhite)
                    Text("Connected", fontSize = 11.sp, color = SeaGreen40)
                }
            }
        } else {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(12.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(DarkGray.copy(alpha = 0.5f))
                    .clickable { onNavigate(Screen.Main) }
                    .padding(14.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                Box(
                    modifier = Modifier
                        .size(38.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(CoralRed40.copy(alpha = 0.3f)),
                    contentAlignment = Alignment.Center
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.HelpOutline,
                        contentDescription = null,
                        tint = CoralRed40,
                        modifier = Modifier.size(20.dp)
                    )
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text("No device", fontSize = 14.sp, fontWeight = FontWeight.Medium, color = OffWhite)
                    Text("Tap to connect", fontSize = 11.sp, color = CoralRed40)
                }
            }
        }

        DrawerItem(Icons.Default.Notifications, "Notifications", AnchorBlue40, onClick = { onNavigate(Screen.Notifications) })
        DrawerItem(Icons.Default.Sms, "SMS sync", AnchorBlue40, onClick = { onNavigate(Screen.Sms) })
        DrawerItem(painterResource(R.drawable.ic_clipboard), "Clipboard", AnchorBlue40, onClick = { onNavigate(Screen.Clipboard) })
        DrawerItem(Icons.Default.Folder, "Files", AnchorBlue40, onClick = { onNavigate(Screen.Files) })
        DrawerItem(painterResource(R.drawable.ic_sailboat), "Sideboat", AnchorBlue40, onClick = { onNavigate(Screen.Sideboat) })
        DrawerItem(painterResource(R.drawable.ic_mouse_pointer), "Remote Input", AnchorBlue40, onClick = { onNavigate(Screen.RemoteInput) })
        DrawerItem(Icons.Default.CameraAlt, "Camera", AnchorBlue40, onClick = { onNavigate(Screen.Camera) })
        DrawerItem(Icons.Default.MusicNote, "Media", AnchorBlue40, onClick = { onNavigate(Screen.Media) })
        DrawerItem(Icons.Default.Terminal, "Commands", AnchorBlue40, onClick = { onNavigate(Screen.Commands) })

        HorizontalDivider(modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp), color = DarkGray)

        DrawerItem(Icons.Default.Settings, "Settings", AnchorGray, onClick = { onNavigate(Screen.Settings) })

        Spacer(Modifier.height(16.dp))
    }
}

@Composable
fun SectionLabel(text: String) {
    Text(
        text = text,
        fontSize = 10.sp,
        fontWeight = FontWeight.SemiBold,
        color = AnchorGray,
        letterSpacing = 1.5.sp,
        modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 16.dp, bottom = 6.dp)
    )
}

@Composable
fun DrawerItem(
    icon: androidx.compose.ui.graphics.painter.Painter,
    label: String,
    tint: Color,
    badge: Int? = null,
    onClick: () -> Unit
) {
    DrawerItemContent(icon = { Icon(painter = icon, contentDescription = label, tint = tint, modifier = Modifier.size(18.dp)) }, label = label, tint = tint, badge = badge, onClick = onClick)
}

@Composable
fun DrawerItem(
    icon: ImageVector,
    label: String,
    tint: Color,
    badge: Int? = null,
    onClick: () -> Unit
) {
    DrawerItemContent(icon = { Icon(imageVector = icon, contentDescription = label, tint = tint, modifier = Modifier.size(18.dp)) }, label = label, tint = tint, badge = badge, onClick = onClick)
}

@Composable
private fun DrawerItemContent(
    icon: @Composable () -> Unit,
    label: String,
    tint: Color,
    badge: Int? = null,
    onClick: () -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 20.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(
            modifier = Modifier
                .size(30.dp)
                .clip(RoundedCornerShape(6.dp))
                .background(DarkGray.copy(alpha = 0.5f)),
            contentAlignment = Alignment.Center
        ) {
            icon()
        }
        Spacer(Modifier.width(14.dp))
        Text(label, fontSize = 14.sp, color = OffWhite, modifier = Modifier.weight(1f))
        if (badge != null && badge > 0) {
            Box(
                modifier = Modifier
                    .size(18.dp)
                    .clip(CircleShape)
                    .background(CoralRed40),
                contentAlignment = Alignment.Center
            ) {
                Text(badge.toString(), fontSize = 10.sp, fontWeight = FontWeight.Bold, color = PureWhite)
            }
        }
    }
}

// ─── Main content ───────────────────────────────────────────────────────

@Composable
fun MainContent(viewModel: MainViewModel, onMenuClick: () -> Unit, onSideboat: () -> Unit) {
    val connectionState by viewModel.connectionState.collectAsState()
    val desktopIp by viewModel.desktopIp.collectAsState()
    val fps by viewModel.videoPlugin.fps.collectAsState()
    val isReceiving by viewModel.videoPlugin.isReceiving.collectAsState()
    val latencyMs by viewModel.latencyMs.collectAsState()
    val desktopBattery by viewModel.desktopBattery.collectAsState()
    val streamStatus by viewModel.videoPlugin.streamStatus.collectAsState()
    val streamError by viewModel.videoPlugin.streamError.collectAsState()
    val availableDisplays by viewModel.videoPlugin.availableDisplays.collectAsState()
    val selectedDisplayIndex by viewModel.videoPlugin.selectedDisplayIndex.collectAsState()
    val deviceList by viewModel.deviceList.collectAsState()
    val isConnected = connectionState.status == ConnectionStatus.CONNECTED
    val context = LocalContext.current

    Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
        // Top bar
        AnchorTopBar(connectionState = connectionState, onMenuClick = onMenuClick, onDisconnect = { viewModel.disconnect() })

        if (isConnected) {
            ConnectedMainPanel(
                isReceiving = isReceiving,
                fps = fps,
                latencyMs = latencyMs,
                desktopBattery = desktopBattery,
                streamStatus = streamStatus,
                streamError = streamError,
                availableDisplays = availableDisplays,
                selectedDisplayIndex = selectedDisplayIndex,
                onSelectDisplay = { viewModel.videoPlugin.selectOutput(it) },
                onStart = { viewModel.videoPlugin.startStreaming() },
                onStop = { viewModel.videoPlugin.stopStreaming() },
                onSideboat = onSideboat
            )
        } else {
            DisconnectedMainPanel(
                connectionState = connectionState,
                desktopIp = desktopIp,
                devices = deviceList,
                onDesktopIpChange = viewModel::setDesktopIp,
                onConnect = { viewModel.connectToPc(desktopIp) },
                onConnectToDevice = { ip -> viewModel.connectToPc(ip) },
                onPairNearby = viewModel::pairNearbyDesktop,
            )
        }
    }
}

@Composable
private fun ConnectedMainPanel(
    isReceiving: Boolean,
    fps: Int,
    latencyMs: Int,
    desktopBattery: Pair<Int, String>?,
    streamStatus: String,
    streamError: String?,
    availableDisplays: List<com.anchor.plugin.VideoPlugin.Display>,
    selectedDisplayIndex: Int,
    onSelectDisplay: (Int) -> Unit,
    onStart: () -> Unit,
    onStop: () -> Unit,
    onSideboat: () -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 20.dp, vertical = 14.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(12.dp)
        ) {
            StatCard(
                value = if (isReceiving) "$fps" else "--",
                label = "FPS",
                color = AnchorBlue40,
                modifier = Modifier.weight(1f)
            )
            StatCard(
                value = if (latencyMs > 0) "${latencyMs}ms" else "--",
                label = "Latency",
                color = SeaGreen40,
                modifier = Modifier.weight(1f)
            )
            if (desktopBattery != null) {
                StatCard(
                    value = "${desktopBattery!!.first}%",
                    label = desktopBattery!!.second,
                    color = when {
                        desktopBattery!!.first <= 20 -> CoralRed40
                        desktopBattery!!.first <= 50 -> OffWhite
                        else -> SeaGreen40
                    },
                    modifier = Modifier.weight(1f)
                )
            }
        }

        Spacer(Modifier.height(16.dp))

        // Stream controls
        StreamControls(
            streamStatus = streamStatus,
            streamError = streamError,
            availableDisplays = availableDisplays,
            selectedDisplayIndex = selectedDisplayIndex,
            onSelectDisplay = onSelectDisplay,
            onStart = onStart,
            onStop = onStop,
            onSideboat = onSideboat
        )

        Spacer(Modifier.weight(1f))
    }
}

@Composable
private fun StreamControls(
    streamStatus: String,
    streamError: String?,
    availableDisplays: List<com.anchor.plugin.VideoPlugin.Display>,
    selectedDisplayIndex: Int,
    onSelectDisplay: (Int) -> Unit,
    onStart: () -> Unit,
    onStop: () -> Unit,
    onSideboat: () -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(6.dp))
            .background(DarkGray.copy(alpha = 0.4f))
            .padding(horizontal = 18.dp, vertical = 16.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                modifier = Modifier
                    .size(36.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(AnchorBlue40.copy(alpha = 0.15f)),
                contentAlignment = Alignment.Center
            ) {
                Icon(
                    painter = painterResource(R.drawable.ic_sailboat),
                    contentDescription = null,
                    tint = AnchorBlue40,
                    modifier = Modifier.size(20.dp)
                )
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text("Sideboat", fontSize = 14.sp, fontWeight = FontWeight.SemiBold, color = OffWhite)
                Text(
                    when (streamStatus) {
                        "idle" -> "Ready"
                        "starting" -> "Starting…"
                        "streaming" -> "Streaming"
                        "switching" -> "Switching screen…"
                        "stopping" -> "Stopping…"
                        "error" -> streamError ?: "Error"
                        else -> streamStatus
                    },
                    fontSize = 11.sp,
                    color = if (streamStatus == "error") CoralRed40 else AnchorGray
                )
            }
            when (streamStatus) {
                "idle", "error" -> Button(
                    onClick = onStart,
                    colors = ButtonDefaults.buttonColors(containerColor = AnchorBlue40),
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp)
                ) {
                    Text("Start", fontSize = 13.sp, color = PureWhite)
                }
                "starting", "switching", "stopping" -> {
                    Button(
                        onClick = {},
                        enabled = false,
                        colors = ButtonDefaults.buttonColors(containerColor = AnchorGray.copy(alpha = 0.3f)),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp)
                    ) {
                        Text("…", fontSize = 13.sp, color = AnchorGray)
                    }
                }
                "streaming" -> {
                    TextButton(onClick = onSideboat) {
                        Text("View", color = AnchorBlue40, fontSize = 12.sp)
                    }
                    Spacer(Modifier.width(4.dp))
                    Button(
                        onClick = onStop,
                        colors = ButtonDefaults.buttonColors(containerColor = CoralRed40),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp)
                    ) {
                        Text("Stop", fontSize = 13.sp, color = PureWhite)
                    }
                }
            }
        }

        // Screen selection below the controls
        if (availableDisplays.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            Text("Screen", fontSize = 11.sp, color = AnchorGray)
            Spacer(Modifier.height(4.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                availableDisplays.forEachIndexed { idx, d ->
                    val sel = idx == selectedDisplayIndex
                    TextButton(
                        onClick = { onSelectDisplay(idx) },
                        colors = ButtonDefaults.textButtonColors(contentColor = AnchorGray),
                        modifier = Modifier
                            .background(DarkGray, RoundedCornerShape(6.dp))
                            .padding(horizontal = 2.dp)
                    ) {
                        Text("${d.name} (${d.width}x${d.height})", fontSize = 10.sp, color = if (sel) OffWhite else AnchorGray)
                    }
                }
            }
        }
    }
}

@Composable
private fun ActionButton(text: String, color: Color, onClick: () -> Unit) {
    TextButton(onClick = onClick) {
        Text(text, color = color, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
}

/** Deep-link to tethering settings; OEMs place it under different activities. */
private fun openTetherSettings(context: Context) {
    val intents = listOf(
        Intent("android.settings.TETHER_SETTINGS"),
        Intent().setComponent(ComponentName("com.android.settings", "com.android.settings.Settings\$TetherSettingsActivity")),
        Intent().setComponent(ComponentName("com.android.settings", "com.android.settings.TetherSettings")),
        Intent(Settings.ACTION_WIRELESS_SETTINGS),
    )
    for (intent in intents) {
        try {
            context.startActivity(intent)
            return
        } catch (_: ActivityNotFoundException) {
        } catch (_: Exception) {
        }
    }
}

@Composable
private fun DisconnectedMainPanel(
    connectionState: com.anchor.data.ConnectionState,
    desktopIp: String,
    devices: List<com.anchor.data.DeviceListEntry>,
    onDesktopIpChange: (String) -> Unit,
    onConnect: () -> Unit,
    onConnectToDevice: (String) -> Unit,
    onPairNearby: (com.anchor.data.DeviceListEntry) -> Unit,
) {
    var nearbyPairingTarget by remember { mutableStateOf<com.anchor.data.DeviceListEntry?>(null) }
    val connecting = connectionState.status == ConnectionStatus.CONNECTING ||
        connectionState.status == ConnectionStatus.PAIRING
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 20.dp, vertical = 14.dp),
        contentAlignment = Alignment.TopCenter
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .widthIn(max = 340.dp)
                .verticalScroll(rememberScrollState())
        ) {
            // --- Discovered + saved devices, grouped by reachability ---
            val nearby = devices.filter { it.isOnline }
            val savedOffline = devices.filter { !it.isOnline }

            if (devices.isEmpty()) {
                Text(
                    "Devices",
                    color = OffWhite,
                    fontSize = 13.sp,
                    modifier = Modifier.padding(start = 2.dp, bottom = 6.dp)
                )
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(6.dp))
                        .background(DarkGray.copy(alpha = 0.4f))
                        .padding(14.dp)
                ) {
                    Text(
                        "Searching for nearby desktops…",
                        color = MediumGray,
                        fontSize = 12.sp
                    )
                }
            }

            if (nearby.isNotEmpty()) {
                DeviceSection(
                    title = "NEARBY",
                    devices = nearby,
                    connecting = connecting,
                    onConnectToDevice = { device ->
                        if (device.isPaired) onConnectToDevice(device.ip) else nearbyPairingTarget = device
                    }
                )
            }

            if (savedOffline.isNotEmpty()) {
                if (nearby.isNotEmpty()) Spacer(Modifier.height(14.dp))
                DeviceSection(
                    title = "SAVED · OFFLINE",
                    devices = savedOffline,
                    connecting = connecting,
                    onConnectToDevice = { device ->
                        if (device.isPaired) onConnectToDevice(device.ip) else nearbyPairingTarget = device
                    }
                )
            }

            Spacer(Modifier.height(18.dp))

            // --- Direct address ---
            Text(
                "CONNECT OR PAIR BY ADDRESS",
                color = OffWhite,
                fontSize = 11.sp,
                letterSpacing = 1.sp,
                modifier = Modifier.padding(start = 2.dp, bottom = 6.dp)
            )
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(6.dp))
                    .background(DarkGray.copy(alpha = 0.4f))
                    .padding(14.dp)
            ) {
                run {
                    val trimmed = desktopIp.trim()
                    val isBlank = trimmed.isBlank()
                    val isValid = run {
                        if (isBlank) return@run false
                        if (trimmed.contains(":") || trimmed.contains("/") || trimmed.contains(" ")) return@run false
                        val ipv4Regex = Regex("^((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$")
                        if (ipv4Regex.matches(trimmed)) return@run true
                        if (trimmed.equals("localhost", ignoreCase = true)) return@run true
                        if (trimmed.matches(Regex("^[0-9]+$"))) return@run false
                        if (trimmed.length < 2) return@run false
                        val hostRegex = Regex("^[A-Za-z0-9]([A-Za-z0-9\\-\\.]*[A-Za-z0-9])?$")
                        if (!hostRegex.matches(trimmed)) return@run false
                        if (!trimmed.contains(".") && trimmed.length < 3) return@run false
                        true
                    }
                    val showError = desktopIp.isNotEmpty() && (isBlank || !isValid)
                    OutlinedTextField(
                        value = desktopIp,
                        onValueChange = onDesktopIpChange,
                        label = { Text("Desktop address") },
                        placeholder = { Text("192.168.1.10 or desktop.tailnet") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true,
                        isError = showError,
                        colors = OutlinedTextFieldDefaults.colors(
                            focusedTextColor = OffWhite,
                            unfocusedTextColor = OffWhite,
                            cursorColor = AnchorBlue40,
                            focusedBorderColor = AnchorBlue40,
                            unfocusedBorderColor = MediumGray,
                            focusedLabelColor = AnchorBlue40,
                            unfocusedLabelColor = MediumGray,
                            errorBorderColor = CoralRed40,
                            errorLabelColor = CoralRed40,
                            errorCursorColor = CoralRed40
                        ),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                        keyboardActions = KeyboardActions(onGo = {
                            if (!isBlank && isValid) onConnect()
                        })
                    )

                    if (showError) {
                        Text(
                            text = if (isBlank) "Enter a desktop address" else "Use an IP, hostname, or Tailscale address — without a port",
                            fontSize = 11.sp,
                            color = CoralRed40,
                            modifier = Modifier.padding(top = 4.dp)
                        )
                    } else if (!connecting) {
                        Text(
                            "Use an alternate LAN or Tailscale address for a paired desktop, or pair a new one.",
                            fontSize = 11.sp,
                            color = MediumGray,
                            modifier = Modifier.padding(top = 6.dp)
                        )
                    }

                    Spacer(Modifier.height(12.dp))

                    Button(
                        onClick = onConnect,
                        enabled = !connecting && !isBlank && isValid,
                        shape = RoundedCornerShape(6.dp),
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Text(
                            if (connecting) "Connecting…" else "Connect / pair"
                        )
                    }
                }

                if (!connectionState.error.isNullOrBlank()) {
                    Spacer(Modifier.height(10.dp))
                    Text(
                        text = connectionState.error!!,
                        fontSize = 11.sp,
                        color = CoralRed40
                    )
                }
            }

        }
    }
    nearbyPairingTarget?.let { device ->
        val safetyNumber = com.anchor.data.pairingSafetyNumber(device.certificateDer)
        AlertDialog(
            onDismissRequest = { nearbyPairingTarget = null },
            title = { Text("Pair with ${device.name}?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "Anchor will ask for approval on ${device.name}. No data is shared unless you accept here and on the desktop.",
                        color = MediumGray,
                    )
                    if (safetyNumber != null) {
                        Spacer(Modifier.height(4.dp))
                        Text("Safety number", color = MediumGray, fontSize = 12.sp)
                        Text(safetyNumber, color = OffWhite, fontFamily = FontFamily.Monospace, fontSize = 24.sp)
                        Text("Accept only if this same number appears on the desktop.", color = MediumGray, fontSize = 12.sp)
                    }
                }
            },
            confirmButton = {
                Button(onClick = {
                    nearbyPairingTarget = null
                    onPairNearby(device)
                }) { Text("Accept & request pairing") }
            },
            dismissButton = { OutlinedButton(onClick = { nearbyPairingTarget = null }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun DeviceSection(
    title: String,
    devices: List<com.anchor.data.DeviceListEntry>,
    connecting: Boolean,
    onConnectToDevice: (com.anchor.data.DeviceListEntry) -> Unit
) {
    Text(
        title,
        color = MediumGray,
        fontSize = 11.sp,
        letterSpacing = 1.sp,
        modifier = Modifier.padding(start = 2.dp, bottom = 6.dp)
    )
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(6.dp))
            .background(DarkGray.copy(alpha = 0.4f))
            .padding(vertical = 4.dp)
    ) {
        devices.forEachIndexed { index, device ->
            DeviceRow(
                device = device,
                enabled = !connecting,
                onClick = { if (device.ip.isNotEmpty()) onConnectToDevice(device) }
            )
            if (index < devices.lastIndex) {
                HorizontalDivider(
                    color = MediumGray.copy(alpha = 0.2f),
                    modifier = Modifier.padding(horizontal = 14.dp)
                )
            }
        }
    }
}

@Composable
private fun DeviceRow(
    device: com.anchor.data.DeviceListEntry,
    enabled: Boolean,
    onClick: () -> Unit
) {
    val tappable = enabled && device.ip.isNotEmpty()
    val contentColor = if (tappable) OffWhite else MediumGray
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = tappable, onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Icon(
            imageVector = Icons.Default.Computer,
            contentDescription = null,
            tint = if (device.isOnline) AnchorBlue40 else MediumGray,
            modifier = Modifier.size(20.dp)
        )
        Spacer(Modifier.width(12.dp))
        Text(
            device.name,
            color = contentColor,
            fontSize = 15.sp
        )
        if (!device.isPaired) {
            Spacer(Modifier.width(8.dp))
            Box(
                modifier = Modifier
                    .clip(RoundedCornerShape(4.dp))
                    .background(AnchorBlue40.copy(alpha = 0.2f))
                    .padding(horizontal = 6.dp, vertical = 1.dp)
            ) {
                Text("NEW", color = AnchorBlue40, fontSize = 9.sp, letterSpacing = 0.5.sp)
            }
        }
        Spacer(Modifier.weight(1f))
        Spacer(Modifier.width(8.dp))
        Text(
            when {
                device.ip.isEmpty() -> "no address"
                device.wired -> "wired · ${device.ip}"
                else -> device.ip
            },
            color = MediumGray,
            fontSize = 11.sp
        )
    }
}

@Composable
private fun StatCard(value: String, label: String, color: Color, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .clip(RoundedCornerShape(6.dp))
            .background(DarkGray.copy(alpha = 0.4f))
            .padding(vertical = 14.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Text(value, fontSize = 22.sp, fontWeight = FontWeight.SemiBold, color = color)
        Spacer(Modifier.height(2.dp))
        Text(label, fontSize = 11.sp, color = AnchorGray)
    }
}

@Composable
private fun SideboatScreen(
    viewModel: MainViewModel,
    onMenuClick: () -> Unit,
    onFullscreen: () -> Unit
) {
    val connectionState by viewModel.connectionState.collectAsState()
    val fps by viewModel.videoPlugin.fps.collectAsState()
    val isReceiving by viewModel.videoPlugin.isReceiving.collectAsState()
    val latencyMs by viewModel.latencyMs.collectAsState()
    val desktopBattery by viewModel.desktopBattery.collectAsState()

    LaunchedEffect(Unit) {
        viewModel.videoPlugin.requestKeyframe()
    }

    Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
        AnchorTopBar(
            connectionState = connectionState,
            onMenuClick = onMenuClick,
            onDisconnect = { viewModel.disconnect() }
        )

        if (connectionState.status == ConnectionStatus.CONNECTED) {
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(12.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .border(1.dp, DarkGray, RoundedCornerShape(6.dp))
                    .background(CharcoalBlack)
            ) {
                VideoSurface(
                    videoPlugin = viewModel.videoPlugin,
                    modifier = Modifier.fillMaxSize(),
                    isFullscreen = false,
                    onToggleFullscreen = onFullscreen
                )

                if (isReceiving) {
                    Box(
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .padding(bottom = 12.dp)
                            .clip(RoundedCornerShape(6.dp))
                            .background(AnchorBlue40.copy(alpha = 0.15f))
                            .padding(horizontal = 16.dp, vertical = 6.dp)
                    ) {
                        Text("Double-tap for fullscreen", fontSize = 12.sp, color = AnchorBlue40)
                    }
                }
            }

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.SpaceAround
            ) {
                StatItem(if (isReceiving) "$fps" else "--", "FPS", AnchorBlue40)
                StatItem(if (latencyMs > 0) "${latencyMs}ms" else "--", "Latency", SeaGreen40)
                // Desktop battery
                val bat = desktopBattery
                if (bat != null) {
                    StatItem(
                        "${bat.first}%",
                        bat.second,
                        when {
                            bat.first <= 20 -> CoralRed40
                            bat.first <= 50 -> OffWhite
                            else -> SeaGreen40
                        }
                    )
                }
            }

            Spacer(Modifier.height(8.dp))
        } else {
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(32.dp),
                contentAlignment = Alignment.Center
            ) {
                Text(
                    "Connect to device to view stream",
                    color = AnchorGray.copy(alpha = 0.8f),
                    fontSize = 15.sp
                )
            }
        }
    }
}

@Composable
fun AnchorTopBar(connectionState: com.anchor.data.ConnectionState, onMenuClick: () -> Unit, onDisconnect: () -> Unit = {}) {
    val statusColor = when (connectionState.status) {
        ConnectionStatus.CONNECTED -> SeaGreen40
        ConnectionStatus.CONNECTING -> AnchorBlue40
        ConnectionStatus.PAIRING -> AnchorBlue40
        ConnectionStatus.DISCONNECTED -> CoralRed40
    }
    val statusText = when (connectionState.status) {
        ConnectionStatus.CONNECTED -> "Connected"
        ConnectionStatus.CONNECTING -> "Connecting..."
        ConnectionStatus.PAIRING -> "Waiting for desktop approval..."
        ConnectionStatus.DISCONNECTED -> "Disconnected"
    }
    val deviceName = if (connectionState.host.isNotEmpty()) connectionState.host else "Anchor"

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 4.dp, end = 16.dp, top = 48.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        IconButton(onClick = onMenuClick) {
            Icon(Icons.Default.Menu, contentDescription = "Menu", tint = AnchorGray)
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(deviceName, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, color = OffWhite)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(statusText, fontSize = 12.sp, color = statusColor)
            }
        }
        if (connectionState.status == ConnectionStatus.CONNECTED) {
            Button(
                onClick = onDisconnect,
                colors = ButtonDefaults.buttonColors(
                    containerColor = DarkGray,
                    contentColor = OffWhite
                ),
                shape = RoundedCornerShape(6.dp),
                modifier = Modifier.height(32.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp, vertical = 0.dp)
            ) {
                Text("Disconnect", fontSize = 12.sp)
            }
        }
    }
}

@Composable
fun StatItem(value: String, label: String, color: Color) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(value, fontSize = 18.sp, fontWeight = FontWeight.SemiBold, color = color)
        Text(label, fontSize = 10.sp, color = MediumGray, letterSpacing = 0.5.sp)
    }
}

// ─── Media Playback ──────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MediaScreen(viewModel: MainViewModel = viewModel(), onBack: () -> Unit = {}, onMenuClick: () -> Unit = {}) {
    val desktopPlayback by viewModel.mediaPlugin.desktopPlayback.collectAsState()
    val phonePlayback by viewModel.mediaPlugin.phonePlayback.collectAsState()
    val desktopArt by viewModel.mediaPlugin.desktopArt.collectAsState()
    val phoneArt by viewModel.mediaPlugin.phoneArt.collectAsState()
    val permissionGranted by viewModel.mediaPlugin.permissionGranted.collectAsState()

    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Media", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp)
        ) {
            // Desktop playback section
            Text(
                "Desktop",
                fontSize = 14.sp,
                fontWeight = FontWeight.Medium,
                color = AnchorBlue40,
                modifier = Modifier.padding(bottom = 8.dp)
            )
            PlaybackCard(playback = desktopPlayback, art = desktopArt, source = "desktop") { cmd ->
                viewModel.mediaPlugin.sendTransportCommand(cmd, "desktop")
            }

            Spacer(Modifier.height(24.dp))

            // Phone playback section
            Text(
                "Phone",
                fontSize = 14.sp,
                fontWeight = FontWeight.Medium,
                color = AnchorBlue40,
                modifier = Modifier.padding(bottom = 8.dp)
            )
            if (!permissionGranted) {
                MediaPermissionCallout(
                    onGrant = { viewModel.mediaPlugin.openNotificationListenerSettings() }
                )
                Spacer(Modifier.height(8.dp))
            }
            PlaybackCard(playback = phonePlayback, art = phoneArt, source = "android") { cmd ->
                viewModel.mediaPlugin.sendTransportCommand(cmd, "android")
            }
        }
    }
}

@Composable
private fun MediaPermissionCallout(onGrant: () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = DeepOcean.copy(alpha = 0.4f)),
        shape = RoundedCornerShape(12.dp)
    ) {
        Column(modifier = Modifier.padding(14.dp)) {
            Text(
                "Notification access required",
                color = OffWhite,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold
            )
            Spacer(Modifier.height(4.dp))
            Text(
                "Anchor needs Notification Listener access to read what's playing on this phone and forward it to the desktop.",
                color = OffWhite.copy(alpha = 0.75f),
                fontSize = 12.sp
            )
            Spacer(Modifier.height(10.dp))
            androidx.compose.material3.TextButton(
                onClick = onGrant,
                colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = AnchorBlue40)
            ) {
                Text("Open settings")
            }
        }
    }
}

@Composable
private fun PlaybackCard(
    playback: MediaPlaybackState?,
    art: android.graphics.Bitmap?,
    source: String,
    onCommand: (String) -> Unit = {}
) {
    if (playback == null) {
        Card(
            modifier = Modifier.fillMaxWidth(),
            colors = CardDefaults.cardColors(containerColor = DarkGray.copy(alpha = 0.35f)),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(
                modifier = Modifier.fillMaxWidth().padding(vertical = 32.dp),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Icon(
                    Icons.Default.MusicNote,
                    contentDescription = null,
                    tint = AnchorGray.copy(alpha = 0.35f),
                    modifier = Modifier.size(40.dp)
                )
                Spacer(Modifier.height(10.dp))
                Text("Nothing playing", color = AnchorGray, fontSize = 14.sp)
            }
        }
        return
    }

    val accent = if (playback.state == "playing") SeaGreen40 else AnchorBlue40

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = DarkGray.copy(alpha = 0.5f)),
        shape = RoundedCornerShape(16.dp)
    ) {
        Column(modifier = Modifier.padding(18.dp)) {
            // App chip (top-right)
            if (playback.app.isNotEmpty()) {
                Row(modifier = Modifier.fillMaxWidth()) {
                    Spacer(Modifier.weight(1f))
                    Surface(
                        shape = RoundedCornerShape(8.dp),
                        color = DeepOcean.copy(alpha = 0.6f)
                    ) {
                        Text(
                            playback.app,
                            fontSize = 11.sp,
                            color = AnchorBlue40,
                            modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                            maxLines = 1
                        )
                    }
                }
                Spacer(Modifier.height(10.dp))
            }

            // Big album art + title
            Row(verticalAlignment = Alignment.CenterVertically) {
                AlbumArtThumbnail(bitmap = art, size = 72.dp)
                Spacer(Modifier.width(14.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        playback.title.ifEmpty { "Unknown" },
                        fontSize = 17.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = OffWhite,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis
                    )
                    if (playback.artist.isNotEmpty()) {
                        Spacer(Modifier.height(2.dp))
                        Text(
                            playback.artist,
                            fontSize = 13.sp,
                            color = OffWhite.copy(alpha = 0.75f),
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )
                    }
                    if (playback.album.isNotEmpty()) {
                        Text(
                            playback.album,
                            fontSize = 12.sp,
                            color = MediumGray,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )
                    }
                }
            }

            Spacer(Modifier.height(16.dp))

            // Progress bar
            if (playback.durationMs > 0) {
                val progress = (playback.positionMs.toFloat() / playback.durationMs).coerceIn(0f, 1f)
                LinearProgressIndicator(
                    progress = { progress },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(4.dp)
                        .clip(RoundedCornerShape(2.dp)),
                    color = accent,
                    trackColor = DarkGray
                )
                Spacer(Modifier.height(6.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text(formatDuration(playback.positionMs), fontSize = 11.sp, color = AnchorGray)
                    Text(formatDuration(playback.durationMs), fontSize = 11.sp, color = AnchorGray)
                }
            }

            Spacer(Modifier.height(12.dp))

            // Transport buttons — circular play/pause in the middle
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically
            ) {
                IconButton(
                    onClick = { onCommand("previous") },
                    enabled = playback.canPrev,
                    modifier = Modifier.size(48.dp)
                ) {
                    Icon(
                        Icons.Default.SkipPrevious,
                        contentDescription = "Previous",
                        tint = if (playback.canPrev) OffWhite else AnchorGray.copy(alpha = 0.3f),
                        modifier = Modifier.size(32.dp)
                    )
                }
                Spacer(Modifier.width(20.dp))
                // Circular play/pause button
                Surface(
                    shape = androidx.compose.foundation.shape.CircleShape,
                    color = accent,
                    modifier = Modifier.size(64.dp)
                ) {
                    IconButton(onClick = {
                        val cmd = if (playback.state == "playing") "pause" else "play"
                        onCommand(cmd)
                    }) {
                        Icon(
                            if (playback.state == "playing") Icons.Default.Pause else Icons.Default.PlayArrow,
                            contentDescription = if (playback.state == "playing") "Pause" else "Play",
                            tint = CharcoalBlack,
                            modifier = Modifier.size(36.dp)
                        )
                    }
                }
                Spacer(Modifier.width(20.dp))
                IconButton(
                    onClick = { onCommand("next") },
                    enabled = playback.canNext,
                    modifier = Modifier.size(48.dp)
                ) {
                    Icon(
                        Icons.Default.SkipNext,
                        contentDescription = "Next",
                        tint = if (playback.canNext) OffWhite else AnchorGray.copy(alpha = 0.3f),
                        modifier = Modifier.size(32.dp)
                    )
                }
            }
        }
    }
}

private fun formatDuration(ms: Long): String {
    val totalSec = (ms / 1000).coerceAtLeast(0)
    val min = totalSec / 60
    val sec = totalSec % 60
    return "%d:%02d".format(min, sec)
}

@Composable
private fun AlbumArtThumbnail(bitmap: android.graphics.Bitmap?, size: androidx.compose.ui.unit.Dp = 56.dp) {
    val cornerRadius = (size.value * 0.14f).dp
    if (bitmap != null) {
        Image(
            bitmap = bitmap.asImageBitmap(),
            contentDescription = "Album art",
            modifier = Modifier
                .size(size)
                .clip(RoundedCornerShape(cornerRadius))
        )
    } else {
        Box(
            modifier = Modifier
                .size(size)
                .clip(RoundedCornerShape(cornerRadius))
                .background(DarkGray),
            contentAlignment = Alignment.Center
        ) {
            Icon(
                Icons.Default.MusicNote,
                contentDescription = null,
                tint = AnchorGray.copy(alpha = 0.4f),
                modifier = Modifier.size(size * 0.45f)
            )
        }
    }
}

// ─── Settings ───────────────────────────────────────────────────────────

@Composable
private fun SmsPermissionControls() {
    val context = LocalContext.current
    val application = context.applicationContext as AnchorApplication
    var showSmsDisclosure by remember { mutableStateOf(false) }
    var showContactsDisclosure by remember { mutableStateOf(false) }
    var smsAccessEnabled by remember { mutableStateOf(application.hasSmsPermissions()) }
    var contactsAccessEnabled by remember { mutableStateOf(application.hasContactsPermission()) }

    val smsPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) {
        smsAccessEnabled = application.hasSmsPermissions()
        if (smsAccessEnabled) application.ensureSmsPluginStarted()
    }
    val contactsPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) {
        contactsAccessEnabled = application.hasContactsPermission()
    }

    if (showSmsDisclosure) {
        AlertDialog(
            onDismissRequest = { showSmsDisclosure = false },
            title = { Text("SMS and MMS access") },
            text = {
                Text(
                    "While a paired desktop is connected, Anchor reads your SMS and MMS conversations " +
                        "and automatically forwards new message content to that desktop, including while " +
                        "Anchor is closed or not in use. Anchor sends messages only when you initiate a " +
                        "reply. Transfers use an encrypted, certificate-authenticated connection. The " +
                        "desktop keeps a local message cache until you clear it there."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        showSmsDisclosure = false
                        smsPermissionLauncher.launch(
                            arrayOf(Manifest.permission.READ_SMS, Manifest.permission.SEND_SMS)
                        )
                    }
                ) { Text("Agree and continue") }
            },
            dismissButton = {
                TextButton(onClick = { showSmsDisclosure = false }) { Text("Not now") }
            }
        )
    }

    if (showContactsDisclosure) {
        AlertDialog(
            onDismissRequest = { showContactsDisclosure = false },
            title = { Text("Contacts access") },
            text = {
                Text(
                    "Anchor optionally reads contact names and photos for phone numbers in your message " +
                        "conversations. Those names and photos are shown in Anchor and sent over the encrypted " +
                        "connection to your paired desktop. You can use SMS sync without contacts access."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        showContactsDisclosure = false
                        contactsPermissionLauncher.launch(Manifest.permission.READ_CONTACTS)
                    }
                ) { Text("Agree and continue") }
            },
            dismissButton = {
                TextButton(onClick = { showContactsDisclosure = false }) { Text("Not now") }
            }
        )
    }

    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(enabled = !smsAccessEnabled) { showSmsDisclosure = true }
                .padding(vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Column(Modifier.weight(1f)) {
                Text("SMS and MMS")
                Text(
                    if (smsAccessEnabled) "Sync enabled on this phone" else "Off until you enable it",
                    style = MaterialTheme.typography.bodySmall,
                    color = AnchorGray
                )
            }
            Text(
                if (smsAccessEnabled) "Allowed" else "Enable",
                color = if (smsAccessEnabled) MaterialTheme.colorScheme.secondary else MaterialTheme.colorScheme.primary
            )
        }
        HorizontalDivider()
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(enabled = !contactsAccessEnabled) { showContactsDisclosure = true }
                .padding(vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Column(Modifier.weight(1f)) {
                Text("Contact names and photos")
                Text(
                    if (contactsAccessEnabled) "Shown with message conversations" else "Optional",
                    style = MaterialTheme.typography.bodySmall,
                    color = AnchorGray
                )
            }
            Text(
                if (contactsAccessEnabled) "Allowed" else "Enable",
                color = if (contactsAccessEnabled) MaterialTheme.colorScheme.secondary else MaterialTheme.colorScheme.primary
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SmsAccessScreen(onBack: () -> Unit = {}, onMenuClick: () -> Unit = {}) {
    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("SMS sync", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = CharcoalBlack)
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            Card(modifier = Modifier.fillMaxWidth().widthIn(max = 640.dp)) {
                Column(modifier = Modifier.padding(18.dp)) {
                    Text("Messages on your desktop", style = MaterialTheme.typography.titleLarge)
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "Choose whether Anchor may sync conversations with desktops you explicitly pair. " +
                            "SMS access is optional and the rest of Anchor works without it.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = AnchorGray
                    )
                    Spacer(Modifier.height(14.dp))
                    SmsPermissionControls()
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    viewModel: MainViewModel = viewModel(),
    onBack: () -> Unit = {},
    onMenuClick: () -> Unit = {},
    onOpenIgnoredApps: () -> Unit = {}
) {
    val context = LocalContext.current
    val connectionState by viewModel.connectionState.collectAsState()
    val pairedDevices by viewModel.pairedDevices.collectAsState()
    val autoReconnect by viewModel.autoReconnectEnabled.collectAsState()
    val usbTetherActive by viewModel.usbTetherActive.collectAsState()

    val statusColor = when (connectionState.status) {
        ConnectionStatus.CONNECTED -> MaterialTheme.colorScheme.secondary
        ConnectionStatus.CONNECTING -> MaterialTheme.colorScheme.primary
        ConnectionStatus.PAIRING -> MaterialTheme.colorScheme.primary
        ConnectionStatus.DISCONNECTED -> MaterialTheme.colorScheme.error
    }
    val statusText = when (connectionState.status) {
        ConnectionStatus.CONNECTED -> "Connected"
        ConnectionStatus.CONNECTING -> "Connecting..."
        ConnectionStatus.PAIRING -> "Waiting for desktop approval..."
        ConnectionStatus.DISCONNECTED -> "Disconnected"
    }

    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Settings", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = CharcoalBlack
                )
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(CharcoalBlack)
                .padding(padding)
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState())
        ) {
            Spacer(Modifier.height(12.dp))

            // Connection Status
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Connection Status", style = MaterialTheme.typography.titleMedium)
                        Text(statusText, style = MaterialTheme.typography.bodyLarge, color = statusColor)
                    }
                    Spacer(Modifier.height(8.dp))
                    HorizontalDivider()
                    Spacer(Modifier.height(8.dp))
                    SettingsRow("Host", if (connectionState.host.isNotEmpty()) connectionState.host else "--")
                    if (connectionState.error != null) {
                        Spacer(Modifier.height(8.dp))
                        Text(connectionState.error!!, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    }
                    HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { openTetherSettings(context) }
                            .padding(vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text("USB tethering")
                            Text(
                                if (usbTetherActive) "On — wired link available"
                                else "Enable for a lower-latency wired link",
                                style = MaterialTheme.typography.bodySmall,
                                color = AnchorGray
                            )
                        }
                        Icon(Icons.Default.ChevronRight, contentDescription = null, tint = AnchorGray)
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // Paired Desktops
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("Paired Desktops", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(12.dp))
                    if (pairedDevices.isEmpty()) {
                        Text("No paired desktops.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.outline)
                    } else {
                        pairedDevices.forEach { device ->
                            Row(
                                modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.SpaceBetween
                            ) {
                                Column(Modifier.weight(1f)) {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Box(modifier = Modifier.size(8.dp).clip(CircleShape).background(
                                            if (device.isOnline) MaterialTheme.colorScheme.secondary else MaterialTheme.colorScheme.outline
                                        ))
                                        Spacer(Modifier.width(8.dp))
                                        Text(device.deviceName, style = MaterialTheme.typography.bodyLarge)
                                    }
                                    Text("ID: ${device.deviceId.take(8)}...", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
                                }
                                Button(
                                    onClick = { viewModel.unpairDevice(device.deviceId) },
                                    colors = ButtonDefaults.buttonColors(
                                        containerColor = DarkGray,
                                        contentColor = OffWhite
                                    ),
                                    shape = RoundedCornerShape(6.dp),
                                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 14.dp, vertical = 6.dp)
                                ) { Text("Unpair", fontSize = 13.sp) }
                            }
                            HorizontalDivider()
                        }
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // Auto-reconnect
            Card(modifier = Modifier.fillMaxWidth()) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text("Auto-reconnect")
                    Switch(checked = autoReconnect, onCheckedChange = { viewModel.setAutoReconnect(it) })
                }
            }

            Spacer(Modifier.height(12.dp))

            // Touch input on stream
            val touchInput by viewModel.touchInputOnStream.collectAsState()
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Touch input on stream")
                        Switch(checked = touchInput, onCheckedChange = { viewModel.setTouchInputOnStream(it) })
                    }
                    Text(
                        "When enabled, tapping the fullscreen video controls the desktop pointer instead of exiting fullscreen.",
                        style = MaterialTheme.typography.bodySmall,
                        color = AnchorGray,
                        modifier = Modifier.padding(top = 4.dp)
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            // Touchpad sensitivity
            val sens by viewModel.touchpadSensitivity.collectAsState()
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Touchpad sensitivity")
                        Text(String.format("%.1fx", sens), style = MaterialTheme.typography.bodyMedium, color = AnchorGray)
                    }
                    Slider(
                        value = sens,
                        onValueChange = { viewModel.setTouchpadSensitivity(it) },
                        valueRange = 0.5f..4.0f,
                        modifier = Modifier.fillMaxWidth()
                    )
                    Text(
                        "Controls pointer speed on the Remote Input touchpad.",
                        style = MaterialTheme.typography.bodySmall,
                        color = AnchorGray
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            // Haptic feedback
            val haptics by viewModel.hapticsEnabled.collectAsState()
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Haptic feedback")
                        Switch(checked = haptics, onCheckedChange = { viewModel.setHapticsEnabled(it) })
                    }
                    Text(
                        "Vibrate on clicks, and tick like a dial while scrolling on the touchpad.",
                        style = MaterialTheme.typography.bodySmall,
                        color = AnchorGray,
                        modifier = Modifier.padding(top = 4.dp)
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            // Sideboat input mode
            val sideboatTouchpad by viewModel.sideboatTouchpadMode.collectAsState()
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Sideboat touchpad mode")
                        Switch(checked = sideboatTouchpad, onCheckedChange = { viewModel.setSideboatTouchpadMode(it) })
                    }
                    Text(
                        "Off: tapping the stream moves the cursor to that exact spot. On: drag like a " +
                            "laptop touchpad — the cursor moves relative to your finger. Switch anytime " +
                            "from the toggle on the fullscreen stream.",
                        style = MaterialTheme.typography.bodySmall,
                        color = AnchorGray,
                        modifier = Modifier.padding(top = 4.dp)
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            // Notifications
            val notifSend by viewModel.notifSendEnabled.collectAsState()
            val notifReceive by viewModel.notifReceiveEnabled.collectAsState()
            val ignoredApps by viewModel.ignoredNotifApps.collectAsState()
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("Notifications", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(8.dp))
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Send phone notifications to desktop", modifier = Modifier.weight(1f))
                        Spacer(Modifier.width(8.dp))
                        Switch(checked = notifSend, onCheckedChange = { viewModel.setNotifSendEnabled(it) })
                    }
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Show desktop notifications on phone", modifier = Modifier.weight(1f))
                        Spacer(Modifier.width(8.dp))
                        Switch(checked = notifReceive, onCheckedChange = { viewModel.setNotifReceiveEnabled(it) })
                    }
                    HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(enabled = notifSend) { onOpenIgnoredApps() }
                            .padding(vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                "Ignored apps",
                                color = if (notifSend) OffWhite else AnchorGray
                            )
                            Text(
                                if (ignoredApps.isEmpty()) "None — all apps forwarded"
                                else "${ignoredApps.size} app${if (ignoredApps.size == 1) "" else "s"} ignored",
                                style = MaterialTheme.typography.bodySmall,
                                color = AnchorGray
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        Icon(
                            Icons.Default.ChevronRight,
                            contentDescription = null,
                            tint = if (notifSend) AnchorGray else AnchorGray.copy(alpha = 0.4f)
                        )
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // Privacy and data
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("Privacy & data", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "Anchor has no accounts, ads, app analytics, or cloud storage for your personal content. " +
                            "Feature data is kept on your devices and sent directly to desktops you approve.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = AnchorGray
                    )
                    HorizontalDivider(modifier = Modifier.padding(vertical = 12.dp))
                    SmsPermissionControls()
                    HorizontalDivider(modifier = Modifier.padding(vertical = 6.dp))
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable {
                                context.startActivity(
                                    Intent(Intent.ACTION_VIEW, Uri.parse("https://anchor-software.org/privacy"))
                                )
                            }
                            .padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Privacy policy", modifier = Modifier.weight(1f))
                        Icon(Icons.Default.ChevronRight, contentDescription = null, tint = AnchorGray)
                    }
                    HorizontalDivider()
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable {
                                context.startActivity(
                                    Intent(Intent.ACTION_VIEW, Uri.parse("https://anchor-software.org/terms"))
                                )
                            }
                            .padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Terms of service", modifier = Modifier.weight(1f))
                        Icon(Icons.Default.ChevronRight, contentDescription = null, tint = AnchorGray)
                    }
                    HorizontalDivider()
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable {
                                val subject = Uri.encode(
                                    "Anchor Android feedback (v${BuildConfig.VERSION_NAME}, ${BuildConfig.GIT_HASH})"
                                )
                                context.startActivity(
                                    Intent(
                                        Intent.ACTION_SENDTO,
                                        Uri.parse("mailto:devs@anchor-software.org?subject=$subject")
                                    )
                                )
                            }
                            .padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text("Support & feedback")
                            Text(
                                "Email devs@anchor-software.org. Please omit private messages and credentials.",
                                style = MaterialTheme.typography.bodySmall,
                                color = AnchorGray
                            )
                        }
                        Icon(Icons.Default.ChevronRight, contentDescription = null, tint = AnchorGray)
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // About
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("About", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(12.dp))
                    SettingsRow("Commit", BuildConfig.GIT_HASH)
                    SettingsRow("Protocol", BuildConfig.PROTOCOL)
                }
            }

            Spacer(Modifier.height(24.dp))
        }
    }
}

// ─── Notifications ──────────────────────────────────────────────────────

private fun isNotificationListenerEnabled(context: android.content.Context): Boolean {
    val flat = Settings.Secure.getString(context.contentResolver, "enabled_notification_listeners") ?: return false
    val component = ComponentName(context, AnchorNotificationListener::class.java).flattenToString()
    return flat.split(':').any { it == component }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NotificationsScreen(viewModel: MainViewModel = viewModel(), onBack: () -> Unit = {}, onMenuClick: () -> Unit = {}) {
    val connectionState by viewModel.connectionState.collectAsState()
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current

    var hasNotifPermission by remember {
        mutableStateOf(
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
            else true
        )
    }
    var hasListenerAccess by remember { mutableStateOf(isNotificationListenerEnabled(context)) }

    // Re-check both permissions when the user returns from the Settings app.
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    hasNotifPermission = ContextCompat.checkSelfPermission(
                        context, Manifest.permission.POST_NOTIFICATIONS
                    ) == PackageManager.PERMISSION_GRANTED
                }
                hasListenerAccess = isNotificationListenerEnabled(context)
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val permLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        hasNotifPermission = granted
    }
    NotificationsScreenContent(
        isConnected = connectionState.status == ConnectionStatus.CONNECTED,
        hasNotifPermission = hasNotifPermission,
        hasListenerAccess = hasListenerAccess,
        onBack = onBack,
        onMenuClick = onMenuClick,
        onSendTestNotification = { viewModel.sendTestFoghornNotification() },
        onRequestPermission = { permLauncher.launch(Manifest.permission.POST_NOTIFICATIONS) },
        onOpenListenerSettings = {
            context.startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS))
        }
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NotificationsScreenContent(
    isConnected: Boolean,
    hasNotifPermission: Boolean,
    hasListenerAccess: Boolean = true,
    onBack: () -> Unit,
    onMenuClick: () -> Unit,
    onSendTestNotification: () -> Unit,
    onRequestPermission: () -> Unit = {},
    onOpenListenerSettings: () -> Unit = {}
) {
    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Notifications", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = CharcoalBlack
                )
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            if (!hasNotifPermission || !hasListenerAccess) {
                Icon(
                    Icons.Default.Notifications,
                    contentDescription = null,
                    tint = AnchorGray.copy(alpha = 0.5f),
                    modifier = Modifier.size(48.dp)
                )
                Spacer(Modifier.height(16.dp))
                Text(
                    "Permissions needed",
                    style = MaterialTheme.typography.titleMedium,
                    color = OffWhite
                )
                Spacer(Modifier.height(16.dp))
                if (!hasNotifPermission) {
                    Text(
                        "Post notifications — lets Anchor display alerts on this device.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = AnchorGray,
                        modifier = Modifier.padding(horizontal = 32.dp)
                    )
                    Spacer(Modifier.height(12.dp))
                    Button(onClick = onRequestPermission) {
                        Text("Grant Notification Permission")
                    }
                    Spacer(Modifier.height(24.dp))
                }
                if (!hasListenerAccess) {
                    Text(
                        "Notification access lets Anchor read notification app names, titles, text, and icons, " +
                            "plus active media titles and playback state. While a paired desktop is connected, " +
                            "Anchor automatically forwards this data over its encrypted connection, including " +
                            "while Anchor is closed or not in use. You can exclude individual apps in Settings.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = AnchorGray,
                        modifier = Modifier.padding(horizontal = 32.dp)
                    )
                    Spacer(Modifier.height(12.dp))
                    Button(onClick = onOpenListenerSettings) {
                        Text("Open Notification Access Settings")
                    }
                }
            } else if (isConnected) {
                Icon(Icons.Default.Notifications, contentDescription = null, tint = AnchorGray.copy(alpha = 0.5f), modifier = Modifier.size(48.dp))
                Spacer(Modifier.height(16.dp))
                Text("Notification sync is active", style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(8.dp))
                Text(
                    "Phone notifications are forwarded to your desktop. Desktop notifications appear here.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = AnchorGray,
                    modifier = Modifier.padding(horizontal = 32.dp)
                )
                Spacer(Modifier.height(24.dp))
                Button(onClick = onSendTestNotification) {
                    Text("Send Test Notification")
                }
            } else {
                Text(
                    "You must be connected to device",
                    color = AnchorGray.copy(alpha = 0.8f),
                    fontSize = 15.sp
                )
            }
        }
    }
}

// ─── Notification app picker ────────────────────────────────────────────

private data class InstalledApp(val pkg: String, val label: String)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NotificationAppsScreen(
    viewModel: MainViewModel,
    onBack: () -> Unit = {},
    onMenuClick: () -> Unit = {}
) {
    val context = LocalContext.current
    val ignored by viewModel.ignoredNotifApps.collectAsState()
    var query by remember { mutableStateOf("") }
    var apps by remember { mutableStateOf<List<InstalledApp>?>(null) }

    LaunchedEffect(Unit) {
        apps = withContext(Dispatchers.IO) {
            val pm = context.packageManager
            pm.getInstalledApplications(0)
                .map { InstalledApp(it.packageName, pm.getApplicationLabel(it).toString()) }
                .sortedBy { it.label.lowercase() }
        }
    }

    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Ignored apps", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = CharcoalBlack
                )
            )
        }
    ) { padding ->
        Column(modifier = Modifier.fillMaxSize().padding(padding)) {
            Text(
                "Checked apps won't forward their notifications to your desktop.",
                style = MaterialTheme.typography.bodySmall,
                color = AnchorGray,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
            )
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text("Search apps") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)
            )
            Spacer(Modifier.height(8.dp))

            val list = apps
            if (list == null) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = AnchorBlue40)
                }
            } else {
                val filtered = remember(list, query) {
                    if (query.isBlank()) list
                    else list.filter {
                        it.label.contains(query, ignoreCase = true) ||
                            it.pkg.contains(query, ignoreCase = true)
                    }
                }
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    items(filtered, key = { it.pkg }) { app ->
                        AppIgnoreRow(
                            app = app,
                            ignored = app.pkg in ignored,
                            onToggle = { viewModel.setAppIgnored(app.pkg, it) }
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun AppIgnoreRow(app: InstalledApp, ignored: Boolean, onToggle: (Boolean) -> Unit) {
    val context = LocalContext.current
    val icon by produceState<androidx.compose.ui.graphics.ImageBitmap?>(null, app.pkg) {
        value = withContext(Dispatchers.IO) {
            try {
                context.packageManager.getApplicationIcon(app.pkg)
                    .toBitmap(width = 96, height = 96)
                    .asImageBitmap()
            } catch (_: Exception) {
                null
            }
        }
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { onToggle(!ignored) }
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        val bmp = icon
        if (bmp != null) {
            Image(bitmap = bmp, contentDescription = null, modifier = Modifier.size(36.dp))
        } else {
            Box(Modifier.size(36.dp))
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(app.label, color = OffWhite, fontSize = 14.sp, maxLines = 1)
            Text(app.pkg, color = AnchorGray, fontSize = 11.sp, maxLines = 1)
        }
        Checkbox(checked = ignored, onCheckedChange = { onToggle(it) })
    }
}

// ─── Remote Input (Touchpad) ────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RemoteInputScreen(viewModel: MainViewModel, onBack: () -> Unit = {}, onMenuClick: () -> Unit = {}) {
    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Remote Input", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = CharcoalBlack
                )
            )
        }
    ) { padding ->
        val sens by viewModel.touchpadSensitivity.collectAsState()
        val hapticsEnabled by viewModel.hapticsEnabled.collectAsState()
        val haptics = rememberHaptics(hapticsEnabled)
        TouchpadSurface(
            inputPlugin = viewModel.inputPlugin,
            sensitivity = sens,
            onSensitivityChange = { viewModel.setTouchpadSensitivity(it) },
            haptics = haptics,
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                // Consume the Scaffold inset so TouchpadSurface's imePadding()
                // lifts the key bar flush onto the keyboard instead of stacking
                // on top of the (unconsumed) nav-bar inset.
                .consumeWindowInsets(padding)
        )
    }
}

// ─── Placeholder ────────────────────────────────────────────────────────

/**
 * CameraX handles all orientation, mirroring, and scaling internally via PreviewView.
 * No manual Matrix transform needed — this is how Signal does it.
 */
@Composable
fun CameraScreen(viewModel: MainViewModel, onBack: () -> Unit, onMenuClick: () -> Unit) {
    val isStreaming by viewModel.cameraPlugin.isStreaming.collectAsState()
    val isSwitching by viewModel.cameraPlugin.isSwitching.collectAsState()
    val sensorOrientation by viewModel.cameraPlugin.sensorOrientation.collectAsState()
    val selectedFacing by viewModel.cameraPlugin.selectedFacing.collectAsState()
    val fps by viewModel.cameraPlugin.fps.collectAsState()
    val streamFps by viewModel.cameraPlugin.streamFps.collectAsState()
    val streamBitrate by viewModel.cameraPlugin.streamBitrate.collectAsState()
    val userOverride by viewModel.cameraPlugin.userOverride.collectAsState()
    val connectionState by viewModel.connectionState.collectAsState()
    val isConnected = connectionState.status == ConnectionStatus.CONNECTED
    val ctx = LocalContext.current
    var cameraPermissionGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
        )
    }
    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted -> cameraPermissionGranted = granted }
    val isFront = selectedFacing == android.hardware.camera2.CameraCharacteristics.LENS_FACING_FRONT

    var settingsExpanded by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        onDispose { viewModel.cameraPlugin.closePreview() }
    }

    Box(modifier = Modifier.fillMaxSize().background(Color.Black)) {

        // ── Full-bleed CameraX PreviewView ────────────────────────────
        if (cameraPermissionGranted) {
            AndroidView(
                factory = { fctx ->
                    androidx.camera.view.PreviewView(fctx).also { previewView ->
                        previewView.implementationMode = androidx.camera.view.PreviewView.ImplementationMode.COMPATIBLE
                        previewView.scaleType = androidx.camera.view.PreviewView.ScaleType.FILL_CENTER
                        viewModel.cameraPlugin.bindPreviewView(previewView, fctx)
                    }
                },
                update = { previewView ->
                    // Re-bind when camera facing changes
                    viewModel.cameraPlugin.bindPreviewView(previewView, ctx)
                },
                modifier = Modifier.fillMaxSize()
            )
        } else {
            Column(
                modifier = Modifier
                    .align(Alignment.Center)
                    .padding(32.dp)
                    .widthIn(max = 420.dp),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Icon(
                    Icons.Default.CameraAlt,
                    contentDescription = null,
                    tint = AnchorGray,
                    modifier = Modifier.size(48.dp)
                )
                Spacer(Modifier.height(16.dp))
                Text("Allow camera access", style = MaterialTheme.typography.titleLarge, color = OffWhite)
                Spacer(Modifier.height(8.dp))
                Text(
                    "Anchor uses the camera only while you preview or stream it to a desktop you approve. " +
                        "Video is sent directly to that paired desktop and is not stored by Anchor.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = AnchorGray
                )
                Spacer(Modifier.height(20.dp))
                Button(onClick = { cameraPermissionLauncher.launch(Manifest.permission.CAMERA) }) {
                    Text("Continue")
                }
            }
        }

        // ── Back button (top-left) ────────────────────────────────────
        FilledIconButton(
            onClick = onBack,
            shape = CircleShape,
            colors = IconButtonDefaults.filledIconButtonColors(
                containerColor = Color.Black,
                contentColor = OffWhite
            ),
            modifier = Modifier
                .align(Alignment.TopStart)
                .padding(16.dp)
                .size(44.dp)
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", modifier = Modifier.size(20.dp))
        }

        // ── Gear (stream settings) top-right ──────────────────────────
        FilledIconButton(
            onClick = { settingsExpanded = !settingsExpanded },
            shape = CircleShape,
            colors = IconButtonDefaults.filledIconButtonColors(
                containerColor = Color.Black,
                contentColor = if (userOverride) CoralRed40 else OffWhite
            ),
            modifier = Modifier
                .align(Alignment.TopEnd)
                .padding(16.dp)
                .size(44.dp)
        ) {
            Icon(Icons.Default.Settings, contentDescription = "Stream settings", modifier = Modifier.size(20.dp))
        }

        // ── Stream settings panel (slides down from top-right) ────────
        if (settingsExpanded) {
            Column(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(top = 68.dp, end = 16.dp, start = 16.dp)
                    .widthIn(max = 320.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(Color.Black.copy(alpha = 0.92f))
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)
            ) {
                Text(
                    "Stream settings",
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = OffWhite
                )

                // FPS dropdown (simple chip row — no dropdown widget needed)
                Text("FPS: $streamFps", fontSize = 12.sp, color = AnchorGray)
                Row(
                    horizontalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    listOf(15, 24, 30, 48, 60).forEach { f ->
                        val selected = streamFps == f
                        Box(
                            modifier = Modifier
                                .clip(RoundedCornerShape(6.dp))
                                .background(if (selected) AnchorBlue40 else Color.White.copy(alpha = 0.08f))
                                .clickable {
                                    viewModel.cameraPlugin.setStreamParamsFromUi(
                                        fps = f,
                                        bitrateBps = streamBitrate
                                    )
                                }
                                .padding(horizontal = 12.dp, vertical = 6.dp),
                            contentAlignment = Alignment.Center
                        ) {
                            Text("$f", fontSize = 12.sp, color = OffWhite)
                        }
                    }
                }

                // Bitrate slider — 500 to 8000 kbps
                val kbps = streamBitrate / 1000
                Text("Bitrate: ${kbps} kbps", fontSize = 12.sp, color = AnchorGray)
                Slider(
                    value = kbps.toFloat(),
                    onValueChange = { v ->
                        viewModel.cameraPlugin.setStreamParamsFromUi(
                            fps = streamFps,
                            bitrateBps = (v.toInt().coerceIn(500, 8000)) * 1000
                        )
                    },
                    valueRange = 500f..8000f,
                    steps = 14, // ~500-kbps steps
                    colors = androidx.compose.material3.SliderDefaults.colors(
                        thumbColor = AnchorBlue40,
                        activeTrackColor = AnchorBlue40,
                        inactiveTrackColor = Color.White.copy(alpha = 0.15f)
                    )
                )

                if (userOverride) {
                    OutlinedButton(
                        onClick = { viewModel.cameraPlugin.clearUserOverride() },
                        modifier = Modifier.fillMaxWidth(),
                        colors = androidx.compose.material3.ButtonDefaults.outlinedButtonColors(
                            contentColor = OffWhite
                        )
                    ) {
                        Text("Use desktop defaults", fontSize = 12.sp)
                    }
                } else {
                    Text(
                        "Using desktop defaults",
                        fontSize = 11.sp,
                        color = AnchorGray
                    )
                }
            }
        }

        // ── Bottom controls overlay ───────────────────────────────────
        Column(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.80f))))
                .padding(horizontal = 32.dp)
                .padding(top = 32.dp, bottom = 40.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(20.dp)
        ) {
            // Status row
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Box(
                    modifier = Modifier
                        .size(8.dp)
                        .clip(CircleShape)
                        .background(when { isStreaming -> SeaGreen40; isConnected -> AnchorBlue40; else -> CoralRed40 })
                )
                Text(
                    text = when { isStreaming -> "$fps fps · streaming"; isConnected -> "Ready to stream"; else -> "Not connected" },
                    fontSize = 13.sp,
                    color = OffWhite
                )
            }

            // 3-col action row: [spacer] [stream button] [flip button]
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                // Left spacer — mirrors flip button size so stream button stays centred
                Spacer(Modifier.size(52.dp))

                // Main stream button
                val streamEnabled = isConnected && !isSwitching
                FilledIconButton(
                    onClick = {
                        if (isStreaming) viewModel.cameraPlugin.stopStreaming()
                        else viewModel.cameraPlugin.startStreaming()
                    },
                    enabled = streamEnabled,
                    shape = CircleShape,
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = if (isStreaming) CoralRed40 else SeaGreen40,
                        contentColor = OffWhite,
                        disabledContainerColor = Color.Gray.copy(alpha = 0.4f),
                        disabledContentColor = OffWhite
                    ),
                    modifier = Modifier.size(72.dp)
                ) {
                    Icon(
                        Icons.Default.CameraAlt,
                        contentDescription = if (isStreaming) "Stop streaming" else "Start streaming",
                        modifier = Modifier.size(28.dp)
                    )
                }

                // Flip button
                FilledIconButton(
                    onClick = { viewModel.cameraPlugin.switchCamera() },
                    enabled = !isSwitching,
                    shape = CircleShape,
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = Color.White.copy(alpha = 0.15f),
                        contentColor = OffWhite,
                        disabledContainerColor = Color.White.copy(alpha = 0.15f),
                        disabledContentColor = OffWhite
                    ),
                    modifier = Modifier.size(52.dp)
                ) {
                    if (isSwitching) {
                        CircularProgressIndicator(modifier = Modifier.size(20.dp), color = OffWhite, strokeWidth = 2.dp)
                    } else {
                        Icon(Icons.Default.FlipCameraAndroid, contentDescription = "Switch camera", modifier = Modifier.size(24.dp))
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PlaceholderScreen(title: String, onBack: () -> Unit = {}, onMenuClick: () -> Unit = {}) {
    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text(title, color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = CharcoalBlack
                )
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Icon(Icons.Default.Settings, contentDescription = null, tint = AnchorGray.copy(alpha = 0.5f), modifier = Modifier.size(48.dp))
            Spacer(Modifier.height(16.dp))
            Text("Coming Soon", style = MaterialTheme.typography.titleMedium, color = AnchorGray)
            Spacer(Modifier.height(8.dp))
            Text(
                "This feature is under development.",
                style = MaterialTheme.typography.bodyMedium,
                color = MediumGray
            )
        }
    }
}

// ─── Pairing dialog (keep) ──────────────────────────────────────────────

@Composable
fun PairingDialog(pairingState: PairingState.Requested, onAccept: () -> Unit, onReject: () -> Unit) {
    AlertDialog(
        onDismissRequest = { onReject() },
        title = { Text("Pairing Request") },
        text = {
            Column {
                Text("A new desktop wants to pair with this device.")
                Spacer(Modifier.height(12.dp))
                Text("Name: ${pairingState.deviceName}", style = MaterialTheme.typography.bodyLarge)
                Text("Type: ${pairingState.deviceType}")
                Spacer(Modifier.height(8.dp))
                Text("Certificate Fingerprint:", style = MaterialTheme.typography.labelMedium)
                Text(pairingState.fingerprint, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                Spacer(Modifier.height(8.dp))
                Text("Verify this fingerprint matches what is shown on the desktop.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
            }
        },
        confirmButton = { Button(onClick = onAccept) { Text("Accept") } },
        dismissButton = { OutlinedButton(onClick = onReject) { Text("Reject") } }
    )
}

// ─── Utility (keep) ─────────────────────────────────────────────────────

@Composable
fun SettingsRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.outline)
        Text(value, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurface)
    }
}

@Preview(name = "Main Connected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun MainConnectedPreview() {
    AnchorTheme(darkTheme = true) {
        Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
            AnchorTopBar(
                connectionState = ConnectionState(
                    status = ConnectionStatus.CONNECTED,
                    host = "studio-mac.local",
                    port = 5027
                ),
                onMenuClick = {},
                onDisconnect = {}
            )
            ConnectedMainPanel(
                isReceiving = true,
                fps = 60,
                latencyMs = 24,
                desktopBattery = null,
                streamStatus = "idle",
                streamError = null,
                availableDisplays = emptyList(),
                selectedDisplayIndex = 0,
                onSelectDisplay = {},
                onStart = {},
                onStop = {},
                onSideboat = {}
            )
        }
    }
}

@Preview(name = "Main Disconnected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun MainDisconnectedPreview() {
    AnchorTheme(darkTheme = true) {
        Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
            AnchorTopBar(
                connectionState = ConnectionState(
                    status = ConnectionStatus.DISCONNECTED,
                    error = "Desktop not reachable"
                ),
                onMenuClick = {}
            )
            DisconnectedMainPanel(
                connectionState = ConnectionState(
                    status = ConnectionStatus.DISCONNECTED,
                    error = "Desktop not reachable"
                ),
                desktopIp = "192.168.1.42",
                devices = listOf(
                    com.anchor.data.DeviceListEntry(
                        deviceId = "abc123",
                        name = "matias-desktop",
                        ip = "192.168.1.42",
                        isPaired = true,
                        isOnline = true
                    ),
                    com.anchor.data.DeviceListEntry(
                        deviceId = "def456",
                        name = "living-room-pc",
                        ip = "192.168.42.10",
                        isPaired = false,
                        isOnline = true,
                        wired = true
                    )
                ),
                onDesktopIpChange = {},
                onConnect = {},
                onConnectToDevice = {},
                onPairNearby = {}
            )
        }
    }
}

@Preview(name = "Drawer", showBackground = true, backgroundColor = 0xFF101417, widthDp = 320, heightDp = 915)
@Composable
private fun DrawerPreview() {
    AnchorTheme(darkTheme = true) {
        ModalDrawerSheet(drawerContainerColor = CharcoalBlack) {
            DrawerContentLayout(
                deviceLabel = "Pixel Preview",
                isConnected = true,
                host = "desk.local",
                drawerOpen = true,
                onNavigate = {}
            )
        }
    }
}

@Preview(name = "Settings", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun SettingsCardPreview() {
    AnchorTheme(darkTheme = true) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(CharcoalBlack)
                .padding(16.dp)
                .verticalScroll(rememberScrollState())
        ) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("Paired Desktops", style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(12.dp))
                    listOf(
                        PairedDeviceDisplay(
                            deviceId = "desktop-alpha-001",
                            deviceName = "Office Mac",
                            fingerprint = "AA:BB:CC",
                            pairedAt = 0,
                            lastSeen = 0,
                            isOnline = true
                        ),
                        PairedDeviceDisplay(
                            deviceId = "desktop-beta-002",
                            deviceName = "Render Box",
                            fingerprint = "DD:EE:FF",
                            pairedAt = 0,
                            lastSeen = 0,
                            isOnline = false
                        )
                    ).forEach { device ->
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween
                        ) {
                            Column(Modifier.weight(1f)) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Box(
                                        modifier = Modifier
                                            .size(8.dp)
                                            .clip(CircleShape)
                                            .background(if (device.isOnline) SeaGreen40 else AnchorGray)
                                    )
                                    Spacer(Modifier.width(8.dp))
                                    Text(device.deviceName, style = MaterialTheme.typography.bodyLarge)
                                }
                                Text(
                                    "ID: ${device.deviceId.take(8)}...",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline
                                )
                            }
                            OutlinedButton(onClick = {}) { Text("Unpair") }
                        }
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}

@Preview(name = "Notifications Connected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun NotificationsPreview() {
    AnchorTheme(darkTheme = true) {
        NotificationsScreenContent(
            isConnected = true,
            hasNotifPermission = true,
            onBack = {},
            onMenuClick = {},
            onSendTestNotification = {}
        )
    }
}

@Preview(name = "Notifications Disconnected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun NotificationsDisconnectedPreview() {
    AnchorTheme(darkTheme = true) {
        NotificationsScreenContent(
            isConnected = false,
            hasNotifPermission = true,
            onBack = {},
            onMenuClick = {},
            onSendTestNotification = {}
        )
    }
}

@Preview(name = "Sideboat Connected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun SideboatPreview() {
    AnchorTheme(darkTheme = true) {
        Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
            AnchorTopBar(
                connectionState = ConnectionState(
                    status = ConnectionStatus.CONNECTED,
                    host = "studio-mac.local",
                    port = 2025
                ),
                onMenuClick = {},
                onDisconnect = {}
            )

            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(12.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .border(1.dp, DarkGray, RoundedCornerShape(6.dp))
                    .background(CharcoalBlack)
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .background(Color.Black)
                )

                Text(
                    "58 fps",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.White,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .padding(12.dp)
                        .clip(RoundedCornerShape(4.dp))
                        .background(Color.Black.copy(alpha = 0.5f))
                        .padding(horizontal = 6.dp, vertical = 2.dp)
                )

                Box(
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .padding(bottom = 12.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(AnchorBlue40.copy(alpha = 0.15f))
                        .padding(horizontal = 16.dp, vertical = 6.dp)
                ) {
                    Text("Double-tap for fullscreen", fontSize = 12.sp, color = AnchorBlue40)
                }
            }

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.SpaceAround
            ) {
                StatItem("58", "FPS", AnchorBlue40)
                StatItem("22ms", "Latency", SeaGreen40)
            }

            Spacer(Modifier.height(8.dp))
        }
    }
}

@Preview(name = "Sideboat Disconnected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun SideboatDisconnectedPreview() {
    AnchorTheme(darkTheme = true) {
        Column(modifier = Modifier.fillMaxSize().background(CharcoalBlack)) {
            AnchorTopBar(
                connectionState = ConnectionState(
                    status = ConnectionStatus.DISCONNECTED,
                    host = "",
                    port = 0
                ),
                onMenuClick = {},
                onDisconnect = {}
            )

            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(32.dp),
                contentAlignment = Alignment.Center
            ) {
                Text(
                    "Connect to device to view stream",
                    color = AnchorGray.copy(alpha = 0.8f),
                    fontSize = 15.sp
                )
            }
        }
    }
}

@Preview(name = "Sideboat Fullscreen", showBackground = true, backgroundColor = 0xFF000000, widthDp = 412, heightDp = 915)
@Composable
private fun SideboatFullscreenPreview() {
    AnchorTheme(darkTheme = true) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
        ) {
            Text(
                "58 fps",
                style = MaterialTheme.typography.labelSmall,
                color = Color.White,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(16.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .background(Color.Black.copy(alpha = 0.5f))
                    .padding(horizontal = 6.dp, vertical = 2.dp)
            )

            Box(
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(16.dp)
                    .size(38.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(CharcoalBlack.copy(alpha = 0.72f)),
                contentAlignment = Alignment.Center
            ) {
                Text("X", fontSize = 14.sp, fontWeight = FontWeight.Bold, color = PureWhite)
            }
        }
    }
}
