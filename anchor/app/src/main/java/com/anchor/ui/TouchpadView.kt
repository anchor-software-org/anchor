package com.anchor.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.anchor.core.MessageBroker
import com.anchor.plugin.InputPlugin
import com.anchor.ui.theme.AnchorBlue40
import com.anchor.ui.theme.AnchorTheme
import com.anchor.ui.theme.CharcoalBlack
import com.anchor.ui.theme.CoralRed40
import com.anchor.ui.theme.DarkGray
import com.anchor.ui.theme.MediumGray
import com.anchor.ui.theme.OffWhite
import com.anchor.ui.theme.SeaGreen40

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun TouchpadSurface(
    inputPlugin: InputPlugin,
    sensitivity: Float = 1.5f,
    onSensitivityChange: (Float) -> Unit = {},
    haptics: Haptics? = null,
    modifier: Modifier = Modifier
) {
    var showKeyboard by remember { mutableStateOf(false) }
    val keyboardController = LocalSoftwareKeyboardController.current
    val focusRequester = remember { FocusRequester() }
    var textBuffer by remember { mutableStateOf("") }

    // Sticky modifier state — toggle on/off, applied to next key typed
    var ctrlActive by remember { mutableStateOf(false) }
    var altActive by remember { mutableStateOf(false) }
    var superActive by remember { mutableStateOf(false) }

    // Helper to get active modifiers and optionally clear them after use
    fun activeModifiers(): List<String> {
        val mods = mutableListOf<String>()
        if (ctrlActive) mods.add("ctrl")
        if (altActive) mods.add("alt")
        if (superActive) mods.add("super")
        return mods
    }

    fun clearModifiers() {
        ctrlActive = false
        altActive = false
        superActive = false
    }

    fun sendWithModifiers(key: String) {
        val mods = activeModifiers()
        if (mods.isNotEmpty()) {
            inputPlugin.sendKeyCombo(mods, key)
            clearModifiers()
        } else {
            inputPlugin.sendKey(key)
        }
    }

    Box(modifier = modifier) {
        // Lift the modifier/quick-key bar to sit flush above the on-screen
        // keyboard. Under edge-to-edge (enableEdgeToEdge + targetSdk 35+) the IME
        // otherwise covers the bar, so taps land on the keyboard instead of the
        // Ctrl/Alt/Super/Esc/Tab/Del buttons. The caller consumes the Scaffold
        // inset (consumeWindowInsets) so this imePadding() lands the bar flush on
        // top of the keyboard rather than double-counting the nav-bar inset.
        Column(
            Modifier
                .fillMaxSize()
                .imePadding()
        ) {
            // Touchpad area
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .background(CharcoalBlack)
                    .pointerInput(Unit) {
                        awaitEachGesture {
                            val down = awaitFirstDown(requireUnconsumed = false)
                            var prevCentroidX = down.position.x
                            var prevCentroidY = down.position.y
                            var prevFingerCount = 1
                            var maxFingerCount = 1
                            var moved = false
                            val tapThreshold = 12f
                            var lastSendTime = 0L
                            // Accumulated scroll distance since the last detent tick.
                            var scrollAccum = 0f
                            val scrollDetentPx = 36f

                            do {
                                val event = awaitPointerEvent()
                                val pointers = event.changes
                                val activePointers = pointers.filter { it.pressed }
                                val currentFingers = activePointers.size
                                maxFingerCount = maxOf(maxFingerCount, currentFingers)

                                if (activePointers.isNotEmpty()) {
                                    // Track centroid of all active fingers
                                    val centroidX = activePointers.map { it.position.x }.average().toFloat()
                                    val centroidY = activePointers.map { it.position.y }.average().toFloat()

                                    // Reset prev on finger count change to avoid jumps
                                    if (currentFingers != prevFingerCount) {
                                        prevCentroidX = centroidX
                                        prevCentroidY = centroidY
                                        prevFingerCount = currentFingers
                                        pointers.forEach { it.consume() }
                                        continue
                                    }

                                    val dx = centroidX - prevCentroidX
                                    val dy = centroidY - prevCentroidY

                                    if (kotlin.math.abs(dx) > 1f || kotlin.math.abs(dy) > 1f) {
                                        moved = true
                                        val now = System.nanoTime()
                                        if (now - lastSendTime >= 8_333_333L) { // ~120 Hz throttle
                                            if (currentFingers >= 2) {
                                                inputPlugin.sendAxis(InputPlugin.AXIS_VERTICAL, dy * 0.8f)
                                                // Dial-like detents: tick each time the scroll
                                                // crosses a fixed distance threshold.
                                                scrollAccum += dy
                                                while (kotlin.math.abs(scrollAccum) >= scrollDetentPx) {
                                                    haptics?.tick()
                                                    scrollAccum -= scrollDetentPx * kotlin.math.sign(scrollAccum)
                                                }
                                            } else {
                                                inputPlugin.sendMotion(dx * sensitivity, dy * sensitivity)
                                            }
                                            lastSendTime = now
                                            prevCentroidX = centroidX
                                            prevCentroidY = centroidY
                                        }
                                    } else {
                                        prevCentroidX = centroidX
                                        prevCentroidY = centroidY
                                    }

                                    pointers.forEach { it.consume() }
                                }
                            } while (pointers.any { it.pressed })

                            if (!moved || (kotlin.math.abs(down.position.x - prevCentroidX) < tapThreshold
                                        && kotlin.math.abs(down.position.y - prevCentroidY) < tapThreshold)) {
                                haptics?.click()
                                if (maxFingerCount >= 2) {
                                    inputPlugin.sendButton(InputPlugin.BTN_RIGHT, true)
                                    inputPlugin.sendButton(InputPlugin.BTN_RIGHT, false)
                                } else {
                                    inputPlugin.sendButton(InputPlugin.BTN_LEFT, true)
                                    inputPlugin.sendButton(InputPlugin.BTN_LEFT, false)
                                }
                            }
                        }
                    }
            ) {
                Column(
                    modifier = Modifier.align(Alignment.Center),
                    horizontalAlignment = Alignment.CenterHorizontally
                ) {
                    Text("Touchpad", fontSize = 16.sp, fontWeight = FontWeight.Medium, color = MediumGray.copy(alpha = 0.3f))
                    Spacer(Modifier.height(4.dp))
                    Text("Drag to move · Tap to click · Two fingers to scroll", fontSize = 11.sp, color = MediumGray.copy(alpha = 0.2f))
                }
            }

            // Bottom bar
            if (!showKeyboard) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(DarkGray)
                        .clickable {
                            showKeyboard = true
                            focusRequester.requestFocus()
                            keyboardController?.show()
                        }
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    contentAlignment = Alignment.Center
                ) {
                    Text(
                        "Touch for keyboard input",
                        fontSize = 13.sp,
                        color = MediumGray.copy(alpha = 0.7f)
                    )
                }
            } else {
                // Keyboard active — down arrow + modifier/quick keys. FlowRow wraps
                // the keys onto a second line so they all stay visible and tappable
                // instead of overflowing off the right edge of a single row.
                FlowRow(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(CharcoalBlack)
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Icon(
                        Icons.Default.KeyboardArrowDown,
                        contentDescription = "Close keyboard",
                        tint = MediumGray,
                        modifier = Modifier
                            .size(24.dp)
                            .clickable {
                                showKeyboard = false
                                keyboardController?.hide()
                            }
                    )

                    ModKey("Ctrl", ctrlActive) { ctrlActive = !ctrlActive }
                    ModKey("Alt", altActive) { altActive = !altActive }
                    ModKey("Super", superActive) { superActive = !superActive }

                    QuickKey("Esc") { sendWithModifiers("Escape") }
                    QuickKey("Tab") { sendWithModifiers("Tab") }
                    QuickKey("Del") { sendWithModifiers("Delete") }
                    QuickKey("Home") { sendWithModifiers("Home") }
                }

                // Invisible text field to capture keyboard input
                Box(Modifier.height(1.dp).fillMaxWidth()) {
                    BasicTextField(
                        value = textBuffer,
                        onValueChange = { new ->
                            val mods = activeModifiers()
                            if (new.length > textBuffer.length) {
                                val added = new.substring(textBuffer.length)
                                if (mods.isNotEmpty()) {
                                    for (ch in added) {
                                        inputPlugin.sendKeyCombo(mods, ch.toString())
                                    }
                                    clearModifiers()
                                } else {
                                    inputPlugin.sendText(added)
                                }
                            } else if (new.length < textBuffer.length) {
                                val deleted = textBuffer.length - new.length
                                repeat(deleted) { inputPlugin.sendKey("BackSpace") }
                            }
                            textBuffer = if (new.length > 500) new.takeLast(500) else new
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(1.dp)
                            .focusRequester(focusRequester)
                            .onKeyEvent { event ->
                                if (event.type == KeyEventType.KeyDown) {
                                    when (event.key) {
                                        Key.Enter -> { sendWithModifiers("Return"); true }
                                        Key.Tab -> { sendWithModifiers("Tab"); true }
                                        Key.Escape -> { sendWithModifiers("Escape"); true }
                                        else -> false
                                    }
                                } else false
                            },
                        textStyle = TextStyle(color = Color.Transparent, fontSize = 1.sp),
                        cursorBrush = SolidColor(Color.Transparent),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                        keyboardActions = KeyboardActions(
                            onSend = {
                                sendWithModifiers("Return")
                                textBuffer = ""
                            }
                        ),
                        singleLine = true
                    )
                }
            }
        }

        LaunchedEffect(showKeyboard) {
            if (showKeyboard) {
                focusRequester.requestFocus()
                keyboardController?.show()
            }
        }
    }
}

/** Sticky modifier key — toggles on/off, highlighted when active. */
@Composable
fun ModKey(label: String, active: Boolean, onClick: () -> Unit) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(if (active) AnchorBlue40.copy(alpha = 0.35f) else DarkGray)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp)
    ) {
        Text(
            label,
            fontSize = 12.sp,
            fontWeight = if (active) FontWeight.Bold else FontWeight.Medium,
            fontFamily = FontFamily.Monospace,
            color = if (active) AnchorBlue40 else MediumGray
        )
    }
}

/** Quick-fire key — sends immediately on tap. */
@Composable
fun QuickKey(label: String, onClick: () -> Unit) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(DarkGray)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp)
    ) {
        Text(
            label,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
            fontFamily = FontFamily.Monospace,
            color = OffWhite
        )
    }
}

@Preview(name = "Remote Input", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun TouchpadSurfacePreview() {
    val inputPlugin = remember { InputPlugin(MessageBroker()) }

    AnchorTheme(darkTheme = true) {
        TouchpadSurface(
            inputPlugin = inputPlugin,
            modifier = Modifier
                .fillMaxSize()
                .background(CharcoalBlack)
        )
    }
}

@Preview(name = "Remote Input – Keyboard Open", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun TouchpadKeyboardPreview() {
    AnchorTheme(darkTheme = true) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(CharcoalBlack)
        ) {
            // Touchpad area
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(horizontal = 2.dp)
                    .background(CharcoalBlack),
                contentAlignment = Alignment.Center
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text("Touchpad", fontSize = 16.sp, fontWeight = FontWeight.Medium, color = MediumGray.copy(alpha = 0.3f))
                    Spacer(Modifier.height(4.dp))
                    Text("Drag to move · Tap to click · Two fingers to scroll", fontSize = 11.sp, color = MediumGray.copy(alpha = 0.2f))
                }
            }

            // Keyboard active — down arrow + keys in one row
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFF1E2128))
                    .padding(horizontal = 8.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Icon(
                    Icons.Default.KeyboardArrowDown,
                    contentDescription = "Close keyboard",
                    tint = MediumGray,
                    modifier = Modifier.size(24.dp)
                )
                ModKey("Ctrl", false) {}
                ModKey("Alt", false) {}
                ModKey("Super", false) {}
                QuickKey("Esc") {}
                QuickKey("Tab") {}
                QuickKey("Del") {}
                QuickKey("Home") {}
                QuickKey("End") {}
            }
        }
    }
}
