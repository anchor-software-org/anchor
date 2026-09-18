package com.anchor.ui

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext

/**
 * Lightweight haptic feedback for the input surfaces: a crisp [click] for
 * button presses and a subtle [tick] used as a scroll "detent". Both no-op when
 * disabled or when the device has no vibrator, so callers can fire freely.
 */
class Haptics(context: Context, var enabled: Boolean = true) {
    private val vibrator: Vibrator? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        (context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager)?.defaultVibrator
    } else {
        @Suppress("DEPRECATION")
        context.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
    }

    private val clickEffect: VibrationEffect? =
        vibrator?.let { VibrationEffect.createPredefined(VibrationEffect.EFFECT_CLICK) }
    private val tickEffect: VibrationEffect? =
        vibrator?.let { VibrationEffect.createPredefined(VibrationEffect.EFFECT_TICK) }

    /** Crisp tap — for a mouse click or button press. */
    fun click() = play(clickEffect)

    /** Subtle detent — fired repeatedly to give scrolling a notched, dial-like feel. */
    fun tick() = play(tickEffect)

    private fun play(effect: VibrationEffect?) {
        val v = vibrator ?: return
        if (!enabled || effect == null || !v.hasVibrator()) return
        v.vibrate(effect)
    }
}

/** Remembers a [Haptics] instance, keeping its [Haptics.enabled] flag in sync. */
@Composable
fun rememberHaptics(enabled: Boolean): Haptics {
    val context = LocalContext.current
    val haptics = remember { Haptics(context, enabled) }
    haptics.enabled = enabled
    return haptics
}
