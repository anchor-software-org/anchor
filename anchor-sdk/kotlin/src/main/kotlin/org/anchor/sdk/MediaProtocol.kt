package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.media.MediaCommand
import org.anchor.sdk.v1.capabilities.media.MediaState

/** Canonical v1 media capability helpers. */
object MediaProtocol {
    data class State(
        val title: String,
        val artist: String,
        val album: String,
        val playing: Boolean,
        val positionMs: Long,
        val durationMs: Long,
        val artworkJpeg: ByteArray,
    )

    data class Command(val kindValue: Int, val positionMs: Long)

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.media"
    const val CAPABILITY_MAJOR = 1
    const val STATE_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.media.MediaState"
    const val COMMAND_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.media.MediaCommand"
    const val MAX_ARTWORK_JPEG_BYTES = 256 * 1024

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME).setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(STATE_TYPE_URL).addRecordTypeUrls(COMMAND_TYPE_URL).build()
    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder()
        .setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeState(
        title: String,
        artist: String,
        album: String,
        playing: Boolean,
        positionMs: Long,
        durationMs: Long,
        artworkJpeg: ByteArray = ByteArray(0),
    ): ByteArray = MediaState.newBuilder()
        .setTitle(title)
        .setArtist(artist)
        .setAlbum(album)
        .setPlaying(playing)
        .setPositionMs(positionMs)
        .setDurationMs(durationMs)
        .setArtworkJpeg(ByteString.copyFrom(artworkJpeg))
        .build()
        .toByteArray()
    fun decodeState(bytes: ByteArray): State = MediaState.parseFrom(bytes).let {
        State(
            title = it.title,
            artist = it.artist,
            album = it.album,
            playing = it.playing,
            positionMs = it.positionMs,
            durationMs = it.durationMs,
            artworkJpeg = it.artworkJpeg.toByteArray(),
        )
    }
    fun encodeCommand(kindValue: Int, positionMs: Long = 0): ByteArray = MediaCommand.newBuilder()
        .setKindValue(kindValue)
        .setPositionMs(positionMs)
        .build()
        .toByteArray()
    fun decodeCommand(bytes: ByteArray): Command = MediaCommand.parseFrom(bytes).let {
        Command(kindValue = it.kindValue, positionMs = it.positionMs)
    }
}
