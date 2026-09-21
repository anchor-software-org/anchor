package org.anchor.sdk

import org.anchor.sdk.v1.capabilities.camera.CameraStatus
import org.anchor.sdk.v1.capabilities.commands.CommandResult
import org.anchor.sdk.v1.capabilities.media.MediaCommand
import org.anchor.sdk.v1.capabilities.media.MediaCommandKind
import org.anchor.sdk.v1.capabilities.media.MediaState
import org.anchor.sdk.v1.capabilities.screen.ScreenOutput
import org.anchor.sdk.v1.capabilities.screen.ScreenOutputList
import org.anchor.sdk.v1.capabilities.screen.ScreenStatus
import org.anchor.sdk.v1.capabilities.sms.SmsMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Round-trip checks for the helpers that are exercised by the less common capabilities. */
class RemainingProtocolTest {
    @Test
    fun cameraStatusRoundTripsAndAdvertisesDatagrams() {
        val status = CameraProtocol.decodeStatus(
            CameraProtocol.encodeStatus(3, 1280, 720, 30, 4000),
        )
        assertEquals(3, status.stateValue)
        assertEquals(1280, status.width)
        assertEquals(720, status.height)
        assertEquals(30, status.fps)
        assertEquals(4000, status.bitrateKbps)
        assertTrue(CameraProtocol.advertisement().supportsDatagrams)
        assertTrue(CameraProtocol.advertisement().recordTypeUrlsList.contains(CameraProtocol.FRAME_TYPE_URL))
    }

    @Test
    fun screenStatusAndOutputListRoundTrip() {
        val status = ScreenStatus.newBuilder()
            .setStateValue(3)
            .setOutputId("display-1")
            .setWidth(1920)
            .setHeight(1080)
            .setFps(60)
            .build()
        val outputs = ScreenOutputList.newBuilder()
            .addOutputs(ScreenOutput.newBuilder().setOutputId("display-1").setDisplayName("HDMI").setWidth(1920).setHeight(1080))
            .build()

        val decodedStatus = ScreenProtocol.decodeStatus(status.toByteArray())
        val decodedOutputs = ScreenProtocol.decodeOutputList(outputs.toByteArray())
        assertEquals(3, decodedStatus.stateValue)
        assertEquals("display-1", decodedStatus.outputId)
        assertEquals(60, decodedStatus.fps)
        assertEquals(1, decodedOutputs.size)
        assertEquals("display-1", decodedOutputs.single().outputId)
        assertEquals("HDMI", decodedOutputs.single().displayName)
        // Sideboat now uses a reliable QUIC stream. A screen capability must
        // not advertise datagrams, or callers can accidentally recreate the
        // lossy/reordering path that caused visible frame degradation.
        assertFalse(ScreenProtocol.advertisement().supportsDatagrams)
    }

    @Test
    fun mediaCommandAndStateRoundTrip() {
        val state = MediaProtocol.decodeState(
            MediaProtocol.encodeState(
                "Track", "Artist", "Album", true, 1200, 3000, byteArrayOf(1, 2, 3),
            ),
        )
        val command = MediaProtocol.decodeCommand(MediaProtocol.encodeCommand(5, 900))

        assertEquals("Track", state.title)
        assertEquals("Artist", state.artist)
        assertEquals("Album", state.album)
        assertTrue(state.playing)
        assertEquals(1200, state.positionMs)
        assertEquals(3000, state.durationMs)
        assertEquals(listOf<Byte>(1, 2, 3), state.artworkJpeg.toList())
        assertEquals(5, command.kindValue)
        assertEquals(900, command.positionMs)
    }

    @Test
    fun smsMessageAndCommandResultRoundTrip() {
        val sms = SmsMessage.newBuilder()
            .setMessageId("m-1")
            .setConversationId("c-1")
            .setAddress("+15551234567")
            .setBody("hello")
            .setTimestampUnixMs(42)
            .setOutgoing(true)
            .build()
        val result = CommandResult.newBuilder()
            .setCommandId("open-url")
            .setExecutionId("e-1")
            .setStatus("completed")
            .setExitCode(0)
            .build()

        val decodedSms = SmsProtocol.decodeMessage(sms.toByteArray())
        assertEquals("m-1", decodedSms.messageId)
        assertEquals("c-1", decodedSms.conversationId)
        assertEquals("+15551234567", decodedSms.address)
        assertEquals("hello", decodedSms.body)
        assertEquals(42, decodedSms.timestampUnixMs)
        assertTrue(decodedSms.outgoing)
        val decoded = CommandsProtocol.decodeResult(result.toByteArray())
        assertEquals("open-url", decoded.commandId)
        assertEquals("e-1", decoded.executionId)
        assertEquals("completed", decoded.status)
        assertEquals(0, decoded.exitCode)
        assertEquals(CommandsProtocol.CAPABILITY_NAME, CommandsProtocol.endpointAdvertisement().capabilitiesList.single().name)
    }

    @Test
    fun everyPublicCapabilityAdvertisesNonEmptyUniqueRecordTypeUrls() {
        val advertisements = listOf(
            CameraProtocol.advertisement(),
            ClipboardProtocol.advertisement(),
            CommandsProtocol.advertisement(),
            DeviceProtocol.advertisement(),
            FilesProtocol.advertisement(),
            InputProtocol.advertisement(),
            MediaProtocol.advertisement(),
            NotificationsProtocol.advertisement(),
            ScreenProtocol.advertisement(),
            SmsProtocol.advertisement(),
        )

        advertisements.forEach { advertisement ->
            assertTrue(advertisement.name.isNotEmpty())
            assertTrue(advertisement.major > 0)
            assertTrue(advertisement.recordTypeUrlsCount > 0)
            assertEquals(
                advertisement.recordTypeUrlsCount,
                advertisement.recordTypeUrlsList.toSet().size,
            )
            advertisement.recordTypeUrlsList.forEach { typeUrl ->
                assertTrue(typeUrl.isNotEmpty())
            }
        }
    }
}
