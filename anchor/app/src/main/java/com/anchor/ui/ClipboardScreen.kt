package com.anchor.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.anchor.plugin.ClipboardHistoryEntry
import com.anchor.plugin.ClipboardPlugin
import com.anchor.ui.theme.*
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ClipboardScreen(
    clipboardPlugin: ClipboardPlugin,
    isConnected: Boolean,
    onBack: () -> Unit
) {
    val history by clipboardPlugin.history.collectAsState()

    ClipboardScreenContent(
        history = history,
        isConnected = isConnected,
        onBack = onBack,
        onClearHistory = clipboardPlugin::clearHistory,
        onSendClipboard = clipboardPlugin::sendCurrentClipboard
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ClipboardScreenContent(
    history: List<ClipboardHistoryEntry>,
    isConnected: Boolean,
    onBack: () -> Unit,
    onClearHistory: () -> Unit,
    onSendClipboard: () -> Unit
) {
    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Clipboard Sync", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = "Back",
                            tint = OffWhite
                        )
                    }
                },
                actions = {
                    if (isConnected && history.isNotEmpty()) {
                        IconButton(onClick = onClearHistory) {
                            Icon(Icons.Default.Delete, "Clear history", tint = AnchorGray)
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = CharcoalBlack)
            )
        },
        bottomBar = {
            if (isConnected) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(CharcoalBlack)
                        .navigationBarsPadding()
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    contentAlignment = Alignment.Center
                ) {
                    Button(
                        onClick = onSendClipboard,
                        modifier = Modifier
                            .widthIn(min = 180.dp, max = 260.dp)
                            .height(46.dp),
                        colors = ButtonDefaults.buttonColors(containerColor = AnchorBlue40),
                        shape = RoundedCornerShape(6.dp)
                    ) {
                        Icon(
                            Icons.AutoMirrored.Filled.Send,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(Modifier.width(8.dp))
                        Text("Send to Desktop", fontSize = 14.sp)
                    }
                }
            }
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
        ) {
            when {
                !isConnected -> {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(32.dp),
                        contentAlignment = Alignment.Center
                    ) {
                        Text(
                            "You must be connected to device",
                            color = AnchorGray.copy(alpha = 0.8f),
                            fontSize = 15.sp
                        )
                    }
                }

                history.isEmpty() -> {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(32.dp),
                        contentAlignment = Alignment.Center
                    ) {
                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                            Text(
                                "Copy text on your desktop or hit send below",
                                color = AnchorGray.copy(alpha = 0.6f),
                                fontSize = 13.sp
                            )
                        }
                    }
                }

                else -> {
                    val listState = rememberLazyListState()

                    LaunchedEffect(history.size) {
                        if (history.isNotEmpty()) listState.animateScrollToItem(0)
                    }

                    LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize(),
                        contentPadding = PaddingValues(vertical = 4.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        items(history, key = { "${it.timestamp}_${it.source}" }) { entry ->
                            ClipboardHistoryCard(entry = entry)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ClipboardHistoryCard(entry: ClipboardHistoryEntry) {
    val isLocal = entry.source == "local"
    val context = LocalContext.current
    val clipboardManager = LocalClipboardManager.current
    val timeStr = remember(entry.timestamp) {
        SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(entry.timestamp))
    }

    val bitmap = remember(entry.imageBytes) {
        entry.imageBytes?.let { bytes ->
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
        }
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(DarkGray)
            .clickable {
                if (entry.isImage && entry.imageBytes != null) {
                    try {
                        val file = java.io.File(context.cacheDir, "clipboard_image.png")
                        file.outputStream().use { out ->
                            out.write(entry.imageBytes)
                        }
                        val uri = androidx.core.content.FileProvider.getUriForFile(
                            context,
                            "${context.packageName}.fileprovider",
                            file
                        )
                        val cm = context.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
                            as android.content.ClipboardManager
                        val clip = android.content.ClipData.newUri(
                            context.contentResolver,
                            "anchor image",
                            uri
                        )
                        cm.setPrimaryClip(clip)
                    } catch (_: Exception) {
                    }
                } else if (entry.text != null) {
                    clipboardManager.setText(AnnotatedString(entry.text))
                }
            }
            .padding(12.dp)
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = if (isLocal) "Sent" else "Received",
                    color = if (isLocal) AnchorBlue40 else SeaGreen40,
                    fontSize = 11.sp
                )
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Text(formatSize(entry.sizeBytes), color = LightGray.copy(alpha = 0.85f), fontSize = 10.sp)
                Text("·", color = LightGray.copy(alpha = 0.55f), fontSize = 10.sp)
                Text(timeStr, color = LightGray.copy(alpha = 0.85f), fontSize = 10.sp)
            }
        }

        Spacer(Modifier.height(8.dp))

        if (entry.isImage && bitmap != null) {
            Image(
                bitmap = bitmap,
                contentDescription = "Clipboard image",
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 200.dp)
                    .clip(RoundedCornerShape(4.dp)),
                contentScale = ContentScale.FillWidth
            )
        } else if (entry.text != null) {
            Text(
                text = entry.text,
                color = OffWhite.copy(alpha = 0.9f),
                fontSize = 13.sp,
                maxLines = 4,
                overflow = TextOverflow.Ellipsis,
                lineHeight = 18.sp
            )
        }
    }
}

@Preview(name = "Clipboard Filled", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun ClipboardScreenPreview() {
    AnchorTheme(darkTheme = true) {
        ClipboardScreenContent(
            history = listOf(
                ClipboardHistoryEntry(
                    text = "ssh hiatus@10.0.0.38\ncargo run --release\n./gradlew installDebug",
                    source = "local",
                    timestamp = 1_712_345_678_000,
                    compressed = true,
                    sizeBytes = 68
                ),
                ClipboardHistoryEntry(
                    text = "Desktop copied a longer block of text so we can verify truncation, spacing, and the card layout in preview mode.",
                    source = "desktop",
                    timestamp = 1_712_345_318_000,
                    compressed = false,
                    sizeBytes = 118
                )
            ),
            isConnected = true,
            onBack = {},
            onClearHistory = {},
            onSendClipboard = {}
        )
    }
}

@Preview(name = "Clipboard Empty", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun ClipboardScreenEmptyPreview() {
    AnchorTheme(darkTheme = true) {
        ClipboardScreenContent(
            history = emptyList(),
            isConnected = true,
            onBack = {},
            onClearHistory = {},
            onSendClipboard = {}
        )
    }
}

@Preview(name = "Clipboard Disconnected", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun ClipboardScreenDisconnectedPreview() {
    AnchorTheme(darkTheme = true) {
        ClipboardScreenContent(
            history = emptyList(),
            isConnected = false,
            onBack = {},
            onClearHistory = {},
            onSendClipboard = {}
        )
    }
}

private fun formatSize(bytes: Int): String = when {
    bytes < 1024 -> "${bytes}B"
    bytes < 1024 * 1024 -> "${bytes / 1024}KB"
    else -> "${"%.1f".format(bytes / (1024.0 * 1024.0))}MB"
}
