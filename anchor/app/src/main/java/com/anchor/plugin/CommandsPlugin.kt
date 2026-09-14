package com.anchor.plugin

import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.json.JSONObject
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.CommandsProtocol
import org.anchor.sdk.AnchorSessionEvent

private const val TAG = "anchor.commands"

/**
 * A command the desktop has published. The phone never learns the underlying
 * shell command string — only enough to display and trigger it by [id].
 */
data class CommandEntry(
    val id: String,
    val name: String,
    val description: String,
    val detach: Boolean
)

/** Latest status update for a triggered command. */
data class CommandResult(
    val id: String,
    val execId: String,
    val name: String,
    val status: Status,
    val exitCode: Int?,
    val error: String?
) {
    enum class Status { STARTED, DONE, FAILED }
}

/**
 * Commands plugin — mirrors Rust's CommandsPlugin.
 *
 * Receives the desktop's command list and triggers execution by id. The phone
 * can only ever send a command's id; it never supplies a command string.
 */
class CommandsPlugin(
    private val broker: MessageBroker
) : Plugin {

    override val pluginId = "commands"

    private var listenJob: Job? = null
    private var scope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null

    fun attachSdkSession(capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK commands capability attached (session=${capability.sessionId})")
    }

    /** Apply a typed result from the desktop command provider. */
    suspend fun handleSdkRecord(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event !is AnchorSessionEvent.CapabilityRecord || event.capabilitySessionId != capability.sessionId) return
        if (event.typeUrl == CommandsProtocol.LIST_TYPE_URL) {
            val list = runCatching { CommandsProtocol.decodeList(event.payload) }.getOrElse {
                Log.w(TAG, "Malformed SDK command list: ${it.message}")
                return
            }
            _commands.value = list.map { entry ->
                CommandEntry(entry.id, entry.name.ifEmpty { entry.id }, entry.description, entry.detach)
            }
            Log.i(TAG, "Received ${_commands.value.size} typed SDK commands from desktop")
            return
        }
        if (event.typeUrl != CommandsProtocol.RESULT_TYPE_URL) return
        val result = runCatching { CommandsProtocol.decodeResult(event.payload) }.getOrElse {
            Log.w(TAG, "Malformed SDK command result: ${it.message}")
            return
        }
        val status = when (result.status) {
            "started" -> CommandResult.Status.STARTED
            "done" -> CommandResult.Status.DONE
            "failed" -> CommandResult.Status.FAILED
            else -> {
                Log.w(TAG, "Unknown SDK command result status: ${result.status}")
                return
            }
        }
        val id = result.commandId
        val execId = result.executionId
        val name = _commands.value.firstOrNull { it.id == id }?.name ?: id
        _lastResult.value = CommandResult(
            id = id,
            execId = execId,
            name = name,
            status = status,
            exitCode = result.exitCode,
            error = result.error.ifEmpty { null },
        )
        Log.i(TAG, "Typed SDK command '$name': ${result.status}")
    }

    private val _commands = MutableStateFlow<List<CommandEntry>>(emptyList())
    val commands: StateFlow<List<CommandEntry>> = _commands.asStateFlow()

    /** Most recent execution status — drives transient UI feedback (snackbar). */
    private val _lastResult = MutableStateFlow<CommandResult?>(null)
    val lastResult: StateFlow<CommandResult?> = _lastResult.asStateFlow()

    override fun start(scope: CoroutineScope) {
        this.scope = scope
        listenJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleIncoming(event)
                }
            }
        }
        Log.i(TAG, "CommandsPlugin started")
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
    }

    /** Ask the desktop to (re)send its command list. */
    fun requestList() {
        send(JSONObject().apply {
            put("plugin_id", pluginId)
            put("type", "list")
        })
    }

    /** Trigger a command by id. */
    fun runCommand(id: String) {
        sdkCapability?.let { capability ->
            scope?.launch(Dispatchers.IO) {
                runCatching { capability.sendRecord(CommandsProtocol.RUN_TYPE_URL, CommandsProtocol.encodeRun(id)) }
                    .onSuccess { Log.i(TAG, "Sent command '$id' through Anchor SDK") }
                    .onFailure { Log.w(TAG, "SDK command send failed: ${it.message}") }
            } ?: Log.w(TAG, "Cannot send command before plugin started")
            return
        }
        send(JSONObject().apply {
            put("plugin_id", pluginId)
            put("type", "run_command")
            put("id", id)
        })
    }

    /** Request the desktop kill a running execution. */
    fun killCommand(execId: String) {
        sdkCapability?.let { capability ->
            scope?.launch(Dispatchers.IO) {
                runCatching { capability.sendRecord(CommandsProtocol.KILL_TYPE_URL, CommandsProtocol.encodeKill(execId)) }
                    .onFailure { Log.w(TAG, "SDK command kill failed: ${it.message}") }
            }
            return
        }
        send(JSONObject().apply {
            put("plugin_id", pluginId)
            put("type", "kill_command")
            put("exec_id", execId)
        })
    }

    /** Clear the transient result (e.g. after showing a snackbar). */
    fun clearLastResult() {
        _lastResult.value = null
    }

    private fun send(json: JSONObject) {
        broker.send(AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(json.toString())))
    }

    private fun handleIncoming(event: AnchorEvent) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return

        val json = try {
            JSONObject(msg.payload)
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse JSON: ${e.message}")
            return
        }

        when (json.optString("type")) {
            "command_list" -> handleCommandList(json)
            "command_result" -> handleCommandResult(json)
        }
    }

    private fun handleCommandList(json: JSONObject) {
        val arr = json.optJSONArray("commands") ?: return
        val list = mutableListOf<CommandEntry>()
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            val id = o.optString("id")
            if (id.isEmpty()) continue
            list.add(
                CommandEntry(
                    id = id,
                    name = o.optString("name", id),
                    description = o.optString("description", ""),
                    detach = o.optBoolean("detach", false)
                )
            )
        }
        Log.i(TAG, "Received ${list.size} commands from desktop")
        _commands.value = list
    }

    private fun handleCommandResult(json: JSONObject) {
        val id = json.optString("id")
        val execId = json.optString("exec_id")
        val statusStr = json.optString("status")
        val status = when (statusStr) {
            "started" -> CommandResult.Status.STARTED
            "done" -> CommandResult.Status.DONE
            "failed" -> CommandResult.Status.FAILED
            else -> {
                Log.w(TAG, "Unknown command result status: $statusStr")
                return
            }
        }
        val name = _commands.value.firstOrNull { it.id == id }?.name ?: id
        val exitCode = if (json.has("exit_code")) json.optInt("exit_code") else null
        val error = json.optString("error", "").ifEmpty { null }

        Log.i(TAG, "Command '$name' ($execId): $statusStr" +
                (exitCode?.let { " code=$it" } ?: "") +
                (error?.let { " error=$it" } ?: ""))

        _lastResult.value = CommandResult(id, execId, name, status, exitCode, error)
    }
}
