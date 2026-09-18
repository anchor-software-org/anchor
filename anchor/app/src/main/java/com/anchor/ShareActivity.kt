package com.anchor

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.core.content.IntentCompat
import com.anchor.data.ConnectionStatus

/**
 * Invisible entry point for the system share sheet. Grabs the shared content
 * URI(s), hands them to [com.anchor.plugin.FileTransferPlugin.sendFile], and
 * finishes immediately — the transfer runs in the process-scoped plugin.
 */
class ShareActivity : ComponentActivity() {

    private companion object { const val TAG = "anchor.share" }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val app = application as AnchorApplication
        app.ensurePluginsStarted()

        val uris = extractUris(intent)
        Log.i(TAG, "Share intent action=${intent.action} type=${intent.type} data=${intent.data} clipData=${intent.clipData?.itemCount} uris=${uris.size} state=${app.connectionState.value.status}")
        when {
            uris.isEmpty() ->
                toast("Nothing to send to your computer")
            app.connectionState.value.status != ConnectionStatus.CONNECTED ->
                toast("Open Anchor and connect to your computer first")
            else -> {
                Log.i(TAG, "Dispatching ${uris.size} URI(s) to FileTransferPlugin")
                uris.forEach { app.fileTransferPlugin.sendFile(it) }
                toast(
                    if (uris.size == 1) "Sending to your computer…"
                    else "Sending ${uris.size} files to your computer…"
                )
            }
        }
        finish()
    }

    private fun extractUris(intent: Intent): List<Uri> = when (intent.action) {
        Intent.ACTION_SEND ->
            listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
                .ifEmpty { intent.clipData?.let { clip -> (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri } } ?: emptyList() }
        Intent.ACTION_SEND_MULTIPLE ->
            IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                ?: intent.clipData?.let { clip -> (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri } }
                ?: emptyList()
        else -> emptyList()
    }

    private fun toast(text: String) =
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show()
}
