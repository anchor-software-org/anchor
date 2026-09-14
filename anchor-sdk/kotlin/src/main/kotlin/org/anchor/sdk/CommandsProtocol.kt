package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.commands.CommandRun
import org.anchor.sdk.v1.capabilities.commands.CommandResult
import org.anchor.sdk.v1.capabilities.commands.CommandKill
import org.anchor.sdk.v1.capabilities.commands.CommandList

/** Typed command requests. The command ID is provider-owned; raw shell text is not on wire. */
object CommandsProtocol {
    data class Definition(
        val id: String,
        val name: String,
        val description: String,
        val detach: Boolean,
    )

    data class Run(val commandId: String, val arguments: List<String>)

    data class Result(
        val commandId: String,
        val executionId: String,
        val status: String,
        val exitCode: Int,
        val error: String,
    )
    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.commands"
    const val CAPABILITY_MAJOR = 1
    const val RUN_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.commands.CommandRun"
    const val RESULT_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.commands.CommandResult"
    const val LIST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.commands.CommandList"
    const val KILL_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.commands.CommandKill"
    fun advertisement() = CapabilityAdvertisement.newBuilder().setName(CAPABILITY_NAME).setMajor(1).addRecordTypeUrls(RUN_TYPE_URL).addRecordTypeUrls(RESULT_TYPE_URL).addRecordTypeUrls(LIST_TYPE_URL).addRecordTypeUrls(KILL_TYPE_URL).build()
    fun endpointAdvertisement() = EndpointAdvertisement.newBuilder().setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeRun(commandId: String, arguments: List<String> = emptyList()) = CommandRun.newBuilder().setCommandId(commandId).addAllArguments(arguments).build().toByteArray()
    fun decodeRun(bytes: ByteArray): Run = CommandRun.parseFrom(bytes)
        .let { Run(it.commandId, it.argumentsList) }
    fun decodeResult(bytes: ByteArray): Result {
        val result = CommandResult.parseFrom(bytes)
        return Result(result.commandId, result.executionId, result.status, result.exitCode, result.error)
    }
    fun decodeList(bytes: ByteArray): List<Definition> = CommandList.parseFrom(bytes).commandsList.map {
        Definition(it.id, it.name, it.description, it.detach)
    }
    fun encodeResult(commandId: String, executionId: String, status: String, exitCode: Int = 0, error: String = "") =
        CommandResult.newBuilder().setCommandId(commandId).setExecutionId(executionId).setStatus(status).setExitCode(exitCode).setError(error).build().toByteArray()
    fun encodeKill(executionId: String) = CommandKill.newBuilder().setExecutionId(executionId).build().toByteArray()
}
