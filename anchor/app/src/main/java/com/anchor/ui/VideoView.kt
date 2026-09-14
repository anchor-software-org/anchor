package com.anchor.ui

import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.anchor.plugin.VideoPlugin

@Composable
fun VideoSurface(
    videoPlugin: VideoPlugin,
    modifier: Modifier = Modifier,
    isFullscreen: Boolean = false,
    onToggleFullscreen: () -> Unit = {}
) {
    val isReceiving by videoPlugin.isReceiving.collectAsState()
    val fps by videoPlugin.fps.collectAsState()

    Box(
        modifier = modifier
            .background(if (isFullscreen) Color.Black else MaterialTheme.colorScheme.background)
            .pointerInput(Unit) {
                detectTapGestures(onDoubleTap = { onToggleFullscreen() })
            }
    ) {
        AndroidView(
            factory = { context ->
                SurfaceView(context).apply {
                    holder.addCallback(object : SurfaceHolder.Callback {
                        override fun surfaceCreated(holder: SurfaceHolder) {
                            videoPlugin.setSurface(holder.surface)
                        }

                        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

                        override fun surfaceDestroyed(holder: SurfaceHolder) {
                            videoPlugin.clearSurface()
                        }
                    })
                }
            },
            modifier = Modifier.fillMaxSize()
        )

        if (!isReceiving) {
            Text(
                text = "Waiting for stream...",
                modifier = Modifier.align(Alignment.Center),
                color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.4f)
            )
        }

        // FPS counter — always visible when receiving
        if (isReceiving) {
            Text(
                text = "$fps fps",
                style = MaterialTheme.typography.labelSmall,
                color = Color.White,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(if (isFullscreen) 16.dp else 12.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .background(Color.Black.copy(alpha = 0.5f))
                    .padding(horizontal = 6.dp, vertical = 2.dp)
            )
        }
    }
}
