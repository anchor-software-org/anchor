package com.anchor

import android.app.Activity
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.os.Bundle
import androidx.core.content.ContextCompat
import com.anchor.plugin.ScreenCaptureService

/**
 * Translucent trampoline that owns the MediaProjection consent dialog.
 *
 * getMediaProjection can only consume the consent result once, and the call
 * must happen inside a foreground service — so this activity just forwards
 * the granted token to ScreenCaptureService and finishes. RemoteInputPlugin
 * learns the outcome through onConsentResult().
 */
class ScreenCaptureConsentActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val mpm = getSystemService(MediaProjectionManager::class.java)
        @Suppress("DEPRECATION")
        startActivityForResult(mpm.createScreenCaptureIntent(), REQUEST_CODE)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val granted = requestCode == REQUEST_CODE && resultCode == RESULT_OK && data != null
        if (granted) {
            val service = Intent(this, ScreenCaptureService::class.java)
                .setAction(ScreenCaptureService.ACTION_START)
                .putExtra(ScreenCaptureService.EXTRA_RESULT_CODE, resultCode)
                .putExtra(ScreenCaptureService.EXTRA_RESULT_DATA, data)
            ContextCompat.startForegroundService(this, service)
        }
        AnchorApplication.instance?.remoteInputPlugin?.onConsentResult(granted)
        finish()
    }

    companion object {
        private const val REQUEST_CODE = 701
    }
}
