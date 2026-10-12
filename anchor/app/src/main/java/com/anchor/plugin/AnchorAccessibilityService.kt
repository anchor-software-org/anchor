package com.anchor.plugin

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Display
import android.view.KeyEvent
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import java.util.concurrent.Executor

private const val TAG = "anchor.remote"

/**
 * Remote-input accessibility service — the stock-Android path for
 * desktop → phone control (the RustDesk / droidVNC-NG model).
 *
 * Enabled once by the user in Settings → Accessibility and persists across
 * reboots. Provides:
 *  - gesture injection via dispatchGesture (tap, drag, swipe)
 *  - global actions (back/home/recents/power)
 *  - text entry via the focused field's InputConnection on API 33+
 *  - still capture via takeScreenshot on API 30+
 *
 * Gesture chaining: Android cancels an in-flight gesture when a new one is
 * dispatched, so drags are modelled as one stroke extended via
 * StrokeDescription.continueStroke(). Motion that arrives while a gesture is
 * still playing is accumulated into pendingDelta and flushed as the next
 * continuation when the previous stroke completes.
 *
 * All entry points are safe to call from any thread; work is marshalled to
 * the service's main thread. Every call is a no-op when the service is not
 * enabled.
 */
class AnchorAccessibilityService : AccessibilityService() {

    private val mainHandler = Handler(Looper.getMainLooper())

    // Gesture pipeline state (main thread only)
    private var gestureBusy = false
    private var heldStroke: GestureDescription.StrokeDescription? = null
    private var pendingDelta: Pair<Float, Float>? = null
    private var pointerX = 0f
    private var pointerY = 0f

    override fun onServiceConnected() {
        instance = this
        Log.i(TAG, "Remote-control accessibility service connected")
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        resetGestureState()
        instance = null
        Log.i(TAG, "Remote-control accessibility service unbound")
        return super.onUnbind(intent)
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}
    override fun onInterrupt() {}

    // ── Pointer ────────────────────────────────────────────────────────

    /** Tap at pixel coordinates on the default display. */
    fun tap(x: Float, y: Float) {
        pointerX = x
        pointerY = y
        post {
            if (gestureBusy) return@post
            val path = Path().apply { moveTo(x, y) }
            dispatchStroke(GestureDescription.StrokeDescription(path, 0L, TAP_MS, false))
        }
    }

    /** Begin a held pointer (button press) — starts a continuing stroke. */
    fun pointerDown(x: Float, y: Float) {
        pointerX = x
        pointerY = y
        post {
            if (gestureBusy || heldStroke != null) return@post
            val path = Path().apply { moveTo(x, y) }
            val stroke = GestureDescription.StrokeDescription(path, 0L, STEP_MS, true)
            heldStroke = stroke
            dispatchStroke(stroke)
        }
    }

    /** Move the held pointer — queues a continuation segment. */
    fun pointerMove(x: Float, y: Float) {
        val dx = x - pointerX
        val dy = y - pointerY
        if (dx == 0f && dy == 0f) return
        pointerX = x
        pointerY = y
        post { queueContinuation(x, y) }
    }

    /** Release the held pointer — finalizes the stroke (touch up). */
    fun pointerUp() {
        post {
            val stroke = heldStroke ?: return@post
            if (gestureBusy) {
                // Defer release until the current segment completes.
                pendingDelta = null
                pendingRelease = true
                return@post
            }
            finishStroke(stroke)
        }
    }

    private var pendingRelease = false

    private fun queueContinuation(x: Float, y: Float) {
        val stroke = heldStroke ?: return
        if (gestureBusy) {
            pendingDelta = x to y
            return
        }
        continueStrokeTo(stroke, x, y)
    }

    private fun continueStrokeTo(stroke: GestureDescription.StrokeDescription, x: Float, y: Float) {
        val (fromX, fromY) = lastStrokeEnd(stroke)
        val path = Path().apply { moveTo(fromX, fromY); lineTo(x, y) }
        val next = stroke.continueStroke(path, 0L, STEP_MS, true)
        heldStroke = next
        dispatchStroke(next)
    }

    private fun finishStroke(stroke: GestureDescription.StrokeDescription) {
        val (fromX, fromY) = lastStrokeEnd(stroke)
        val path = Path().apply { moveTo(fromX, fromY); lineTo(pointerX, pointerY) }
        heldStroke = null
        dispatchStroke(stroke.continueStroke(path, 0L, TAP_MS, false))
    }

    // StrokeDescription doesn't expose its end point; track it alongside.
    private var strokeEndX = 0f
    private var strokeEndY = 0f
    private fun lastStrokeEnd(stroke: GestureDescription.StrokeDescription): Pair<Float, Float> =
        strokeEndX to strokeEndY

    private fun dispatchStroke(stroke: GestureDescription.StrokeDescription) {
        gestureBusy = true
        // Track where this stroke's path ends — continueStroke requires the
        // next segment to start exactly there.
        strokeEndX = pointerX
        strokeEndY = pointerY
        val gesture = GestureDescription.Builder()
            .addStroke(stroke)
            .setDisplayId(Display.DEFAULT_DISPLAY)
            .build()
        val ok = dispatchGesture(gesture, object : GestureResultCallback() {
            override fun onCompleted(gestureDescription: GestureDescription?) {
                gestureBusy = false
                onStrokeSettled()
            }
            override fun onCancelled(gestureDescription: GestureDescription?) {
                gestureBusy = false
                resetGestureState()
            }
        }, mainHandler)
        if (!ok) {
            gestureBusy = false
            resetGestureState()
            Log.w(TAG, "dispatchGesture rejected")
        }
    }

    private fun onStrokeSettled() {
        when {
            pendingRelease -> {
                pendingRelease = false
                heldStroke?.let { finishStroke(it) }
            }
            pendingDelta != null -> {
                val (x, y) = pendingDelta!!
                pendingDelta = null
                heldStroke?.let { continueStrokeTo(it, x, y) }
            }
        }
    }

    private fun resetGestureState() {
        heldStroke = null
        pendingDelta = null
        pendingRelease = false
        gestureBusy = false
    }

    /**
     * Scroll via a swipe stroke opposite the scroll direction, centred on the
     * current pointer position. Units are pixels to travel.
     */
    fun scroll(dx: Float, dy: Float) {
        val cx = pointerX
        val cy = pointerY
        post {
            if (gestureBusy || heldStroke != null) return@post
            // StrokeDescription throws on any path point outside the display —
            // a big wheel delta near the edge would otherwise crash the app.
            val (w, h) = displaySize()
            val endX = (cx - dx).coerceIn(0f, w.toFloat())
            val endY = (cy - dy).coerceIn(0f, h.toFloat())
            val path = Path().apply { moveTo(cx, cy); lineTo(endX, endY) }
            runCatching {
                dispatchStroke(GestureDescription.StrokeDescription(path, 0L, SCROLL_MS, false))
            }.onFailure { Log.w(TAG, "scroll stroke rejected: ${it.message}") }
        }
    }

    // ── Global actions ─────────────────────────────────────────────────

    fun globalAction(action: Int) {
        post { performGlobalAction(action) }
    }

    /** Map a USB HID usage ID to an available action/key event. */
    fun hidKey(hidUsage: Int, pressed: Boolean) {
        if (!pressed) return
        post {
            when (hidUsage) {
                0x29 -> performGlobalAction(GLOBAL_ACTION_BACK)   // Escape → Back
                0x4a -> performGlobalAction(GLOBAL_ACTION_HOME)   // Home
                0x28 -> sendKeyEvent(KeyEvent.KEYCODE_ENTER)      // Enter
                0x2a -> sendKeyEvent(KeyEvent.KEYCODE_DEL)        // Backspace
                0x2b -> sendKeyEvent(KeyEvent.KEYCODE_TAB)        // Tab
                0x4f -> sendKeyEvent(KeyEvent.KEYCODE_DPAD_RIGHT)
                0x50 -> sendKeyEvent(KeyEvent.KEYCODE_DPAD_LEFT)
                0x51 -> sendKeyEvent(KeyEvent.KEYCODE_DPAD_DOWN)
                0x52 -> sendKeyEvent(KeyEvent.KEYCODE_DPAD_UP)
                else -> Log.d(TAG, "unmapped hid usage $hidUsage")
            }
        }
    }

    private fun sendKeyEvent(keyCode: Int) {
        if (Build.VERSION.SDK_INT >= 33) {
            val ic = inputMethod?.currentInputConnection
            if (ic != null) {
                ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, keyCode))
                ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_UP, keyCode))
                return
            }
        }
        Log.d(TAG, "no input connection for key event $keyCode")
    }

    // ── Text ───────────────────────────────────────────────────────────

    /**
     * Commit text into the focused field. API 33+ uses the service's
     * InputConnection (RustDesk approach — no companion IME needed). Older
     * APIs fall back to appending via ACTION_SET_TEXT on the focused editable
     * node.
     */
    fun commitText(text: String) {
        post {
            if (Build.VERSION.SDK_INT >= 33) {
                val ic = inputMethod?.currentInputConnection
                if (ic != null) {
                    ic.commitText(text, 1, null)
                    return@post
                }
            }
            val node = findFocusedEditable() ?: run {
                Log.d(TAG, "commitText: no focused editable node")
                return@post
            }
            val args = Bundle().apply {
                putCharSequence(
                    AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                    (node.text?.toString() ?: "") + text
                )
            }
            node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
            node.recycle()
        }
    }

    private fun findFocusedEditable(): AccessibilityNodeInfo? {
        val root = rootInActiveWindow ?: return null
        val node = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
            ?: root.findFocus(AccessibilityNodeInfo.FOCUS_ACCESSIBILITY)
        return node?.takeIf { it.isEditable }
    }

    // ── Screenshot ─────────────────────────────────────────────────────

    /** Zero-consent still capture (API 30+). Slow — a fallback view, not video. */
    fun grabScreenshot(
        executor: Executor,
        callback: TakeScreenshotCallback
    ) {
        if (Build.VERSION.SDK_INT >= 30) {
            post { takeScreenshot(Display.DEFAULT_DISPLAY, executor, callback) }
        }
    }

    // ── Metrics / state ────────────────────────────────────────────────

    fun displaySize(): Pair<Int, Int> {
        val dm = resources.displayMetrics
        return dm.widthPixels to dm.heightPixels
    }

    fun pointerPosition(): Pair<Float, Float> = pointerX to pointerY

    private fun post(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) block() else mainHandler.post(block)
    }

    companion object {
        // Short segments = taps complete fast and drag continuations track
        // at ~40Hz instead of stepping in 100ms chunks.
        private const val TAP_MS = 30L
        private const val STEP_MS = 25L
        private const val SCROLL_MS = 150L

        @Volatile
        private var instance: AnchorAccessibilityService? = null

        fun get(): AnchorAccessibilityService? = instance
        fun isEnabled(): Boolean = instance != null
    }
}
