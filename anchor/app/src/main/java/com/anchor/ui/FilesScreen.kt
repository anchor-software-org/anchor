package com.anchor.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Size
import android.widget.Toast
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.Movie
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material3.*
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.anchor.plugin.FileTransferPlugin
import com.anchor.plugin.TransferDirection
import com.anchor.plugin.TransferRecord
import com.anchor.ui.theme.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FilesScreen(
    fileTransferPlugin: FileTransferPlugin,
    onBack: () -> Unit,
) {
    val history by fileTransferPlugin.history.collectAsState()
    FilesScreenContent(
        history = history,
        onBack = onBack,
        onClear = fileTransferPlugin::clearHistory,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FilesScreenContent(
    history: List<TransferRecord>,
    onBack: () -> Unit,
    onClear: () -> Unit,
) {
    val received = history.filter { it.direction == TransferDirection.RECEIVED }
    val sent = history.filter { it.direction == TransferDirection.SENT }

    Scaffold(
        containerColor = CharcoalBlack,
        topBar = {
            TopAppBar(
                title = { Text("Files", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back", tint = OffWhite)
                    }
                },
                actions = {
                    if (history.isNotEmpty()) {
                        IconButton(onClick = onClear) {
                            Icon(Icons.Default.Delete, "Clear list", tint = AnchorGray)
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = CharcoalBlack)
            )
        }
    ) { padding ->
        if (history.isEmpty()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .padding(32.dp),
                contentAlignment = Alignment.Center
            ) {
                Text(
                    "Files you send or receive appear here.\nShare a file to \"Anchor\" to send it to your computer.",
                    color = AnchorGray.copy(alpha = 0.6f),
                    fontSize = 13.sp,
                    textAlign = TextAlign.Center
                )
            }
        } else {
            val gridState = rememberLazyGridState()
            LazyVerticalGrid(
                state = gridState,
                columns = GridCells.Adaptive(minSize = 78.dp),
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentPadding = PaddingValues(start = 10.dp, end = 10.dp, top = 4.dp, bottom = 20.dp),
                horizontalArrangement = Arrangement.spacedBy(7.dp),
                verticalArrangement = Arrangement.spacedBy(7.dp)
            ) {
                section("Received", received.size, SeaGreen40)
                items(received, key = { "r_${it.timestamp}_${it.name}" }) { FileTile(it) }

                section("Sent", sent.size, AnchorBlue40)
                items(sent, key = { "s_${it.timestamp}_${it.name}" }) { FileTile(it) }
            }
        }
    }
}

/** A full-width section header inside the grid (no-op when the group is empty). */
private fun androidx.compose.foundation.lazy.grid.LazyGridScope.section(
    label: String,
    count: Int,
    accent: androidx.compose.ui.graphics.Color,
) {
    if (count == 0) return
    item(span = { GridItemSpan(maxLineSpan) }, key = "hdr_$label") {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 14.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = label.uppercase(),
                color = accent,
                fontSize = 11.sp,
                fontWeight = FontWeight.SemiBold,
                letterSpacing = 1.sp
            )
            Spacer(Modifier.width(6.dp))
            Text(count.toString(), color = AnchorGray.copy(alpha = 0.6f), fontSize = 11.sp)
        }
    }
}

@Composable
private fun FileTile(rec: TransferRecord) {
    val context = LocalContext.current
    val thumb = rememberThumbnail(rec.uri, rec.mime)

    Column(
        modifier = Modifier
            .clip(RoundedCornerShape(12.dp))
            .background(DarkGray)
            .clickable { openFile(context, rec.uri, rec.mime) }
    ) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .aspectRatio(1f)
                .background(CharcoalBlack),
            contentAlignment = Alignment.Center
        ) {
            if (thumb != null) {
                Image(
                    bitmap = thumb,
                    contentDescription = rec.name,
                    modifier = Modifier.fillMaxSize(),
                    contentScale = ContentScale.Crop
                )
            } else {
                Icon(
                    imageVector = typeIcon(rec.mime),
                    contentDescription = null,
                    tint = OffWhite.copy(alpha = 0.4f),
                    modifier = Modifier.size(26.dp)
                )
            }
        }

        Text(
            text = rec.name,
            color = OffWhite.copy(alpha = 0.85f),
            fontSize = 9.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(horizontal = 5.dp, vertical = 4.dp)
        )
    }
}

/** Loads a MediaStore thumbnail for image/video URIs; null for everything else. */
@Composable
private fun rememberThumbnail(uri: Uri?, mime: String?): ImageBitmap? {
    val context = LocalContext.current
    var bitmap by remember(uri) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(uri, mime) {
        val isMedia = mime?.startsWith("image/") == true || mime?.startsWith("video/") == true
        bitmap = if (uri == null || !isMedia) {
            null
        } else {
            withContext(Dispatchers.IO) {
                runCatching {
                    context.contentResolver.loadThumbnail(uri, Size(300, 300), null).asImageBitmap()
                }.getOrNull()
            }
        }
    }
    return bitmap
}

/** Icon shown for files without a thumbnail, by broad MIME kind. */
private fun typeIcon(mime: String?): ImageVector = when {
    mime?.startsWith("image/") == true -> Icons.Default.Image
    mime?.startsWith("video/") == true -> Icons.Default.Movie
    mime?.startsWith("audio/") == true -> Icons.Default.MusicNote
    else -> Icons.Default.Description
}

private fun openFile(context: Context, uri: Uri?, mime: String?) {
    if (uri == null) {
        Toast.makeText(context, "This file is no longer available", Toast.LENGTH_SHORT).show()
        return
    }
    try {
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, mime ?: "*/*")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        context.startActivity(Intent.createChooser(intent, "Open with"))
    } catch (_: Exception) {
        Toast.makeText(context, "No app can open this file", Toast.LENGTH_SHORT).show()
    }
}

@Preview(showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun FilesScreenPreview() {
    AnchorTheme(darkTheme = true) {
        FilesScreenContent(
            history = listOf(
                TransferRecord("photo_2026.jpg", TransferDirection.RECEIVED, 2_400_000, 1_712_345_678_000, null, "image/jpeg"),
                TransferRecord("clip.mp4", TransferDirection.RECEIVED, 14_800_000, 1_712_345_218_000, null, "video/mp4"),
                TransferRecord("readme.txt", TransferDirection.RECEIVED, 1_200, 1_712_345_200_000, null, "text/plain"),
                TransferRecord("notes.pdf", TransferDirection.SENT, 88_000, 1_712_345_318_000, null, "application/pdf"),
                TransferRecord("archive.zip", TransferDirection.SENT, 5_100_000, 1_712_345_118_000, null, "application/zip"),
            ),
            onBack = {},
            onClear = {},
        )
    }
}

@Preview(name = "Files Empty", showBackground = true, backgroundColor = 0xFF101417, widthDp = 412, heightDp = 915)
@Composable
private fun FilesScreenEmptyPreview() {
    AnchorTheme(darkTheme = true) {
        FilesScreenContent(history = emptyList(), onBack = {}, onClear = {})
    }
}
