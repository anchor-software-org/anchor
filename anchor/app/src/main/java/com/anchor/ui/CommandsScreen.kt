package com.anchor.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.anchor.plugin.CommandEntry
import com.anchor.plugin.CommandResult
import com.anchor.plugin.CommandsPlugin
import com.anchor.ui.theme.*

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CommandsScreen(
    commandsPlugin: CommandsPlugin,
    isConnected: Boolean,
    onBack: () -> Unit,
    onMenuClick: () -> Unit
) {
    val commands by commandsPlugin.commands.collectAsState()
    val lastResult by commandsPlugin.lastResult.collectAsState()
    val snackbarHostState = remember { SnackbarHostState() }

    // Refresh the list when the screen opens and a device is connected.
    LaunchedEffect(isConnected) {
        if (isConnected) commandsPlugin.requestList()
    }

    // Surface execution status as a snackbar.
    LaunchedEffect(lastResult) {
        val r = lastResult ?: return@LaunchedEffect
        val text = when (r.status) {
            CommandResult.Status.STARTED -> "${r.name}: started"
            CommandResult.Status.DONE -> "${r.name}: done"
            CommandResult.Status.FAILED ->
                "${r.name}: failed" + (r.error?.let { " ($it)" }
                    ?: r.exitCode?.let { " (exit $it)" } ?: "")
        }
        snackbarHostState.showSnackbar(text)
        commandsPlugin.clearLastResult()
    }

    Scaffold(
        containerColor = CharcoalBlack,
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            TopAppBar(
                title = { Text("Commands", color = OffWhite) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", tint = OffWhite)
                    }
                },
                actions = {
                    IconButton(onClick = onMenuClick) {
                        Icon(Icons.Default.Menu, contentDescription = "Menu", tint = OffWhite)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = CharcoalBlack)
            )
        }
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
        ) {
            when {
                !isConnected -> CenteredHint("Connect to a desktop to use commands.")
                commands.isEmpty() -> CenteredHint("No commands yet.\nAdd them on the desktop.")
                else -> LazyVerticalGrid(
                    columns = GridCells.Adaptive(minSize = 160.dp),
                    contentPadding = PaddingValues(12.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                    modifier = Modifier.fillMaxSize()
                ) {
                    items(commands, key = { it.id }) { cmd ->
                        CommandCard(cmd = cmd, onClick = { commandsPlugin.runCommand(cmd.id) })
                    }
                }
            }
        }
    }
}

@Composable
private fun CommandCard(cmd: CommandEntry, onClick: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(DarkGray.copy(alpha = 0.5f))
            .clickable(onClick = onClick)
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(DeepOcean),
                contentAlignment = Alignment.Center
            ) {
                Icon(
                    Icons.Default.PlayArrow,
                    contentDescription = null,
                    tint = PureWhite,
                    modifier = Modifier.size(20.dp)
                )
            }
            Spacer(Modifier.width(10.dp))
            Text(
                cmd.name,
                color = OffWhite,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f)
            )
        }
        if (cmd.description.isNotEmpty()) {
            Text(
                cmd.description,
                color = AnchorGray,
                fontSize = 12.sp,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis
            )
        }
        if (cmd.detach) {
            Text("detached", color = AnchorBlue40, fontSize = 10.sp, fontWeight = FontWeight.Medium)
        }
    }
}

@Composable
private fun CenteredHint(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(text, color = AnchorGray, fontSize = 14.sp)
    }
}
