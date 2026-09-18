package com.anchor.core

import android.util.Log

private const val TAG = "anchor"
private const val HEADER_SIZE = 12
private const val STALE_TIMEOUT_NS = 100_000_000L // 100ms
private const val KEYFRAME_MIN_INTERVAL_NS = 3_000_000_000L // 3 seconds
private const val KEYFRAME_MIN_SKIPPED_FRAMES = 3L

/**
 * Reassembles H264 frames from fragmented UDP packets.
 *
 * UDP packet header (12 bytes, little-endian):
 *   [frame_id: u32][fragment_index: u16][fragment_count: u16][frame_size: u32]
 */
class UdpFrameReassembler(
    private val onFrameComplete: (ByteArray) -> Unit,
    private val onFrameLost: () -> Unit
) {
    private class FrameAssembly(
        val fragmentCount: Int,
        val frameSize: Int,
        val fragments: Array<ByteArray?>,
        var receivedCount: Int = 0,
        val createdAt: Long = System.nanoTime()
    )

    private val assemblies = HashMap<Long, FrameAssembly>()
    private var lastCompletedFrameId: Long = 0
    private var totalFrames = 0L
    private var totalLost = 0L
    private var lastKeyframeRequestNs: Long = 0

    fun onPacket(data: ByteArray, length: Int) {
        if (length < HEADER_SIZE) return

        // Parse header (little-endian)
        val frameId = (data[0].toInt() and 0xFF) or
                ((data[1].toInt() and 0xFF) shl 8) or
                ((data[2].toInt() and 0xFF) shl 16) or
                ((data[3].toInt() and 0xFF) shl 24)
        val frameIdLong = frameId.toLong() and 0xFFFFFFFFL

        val fragIndex = (data[4].toInt() and 0xFF) or ((data[5].toInt() and 0xFF) shl 8)
        val fragCount = (data[6].toInt() and 0xFF) or ((data[7].toInt() and 0xFF) shl 8)
        val frameSize = (data[8].toInt() and 0xFF) or
                ((data[9].toInt() and 0xFF) shl 8) or
                ((data[10].toInt() and 0xFF) shl 16) or
                ((data[11].toInt() and 0xFF) shl 24)

        if (fragCount == 0 || fragIndex >= fragCount) return
        if (frameIdLong <= lastCompletedFrameId) return // stale

        val payloadLen = length - HEADER_SIZE
        if (payloadLen <= 0) return

        val assembly = assemblies.getOrPut(frameIdLong) {
            FrameAssembly(
                fragmentCount = fragCount,
                frameSize = frameSize,
                fragments = arrayOfNulls(fragCount)
            )
        }

        if (fragIndex >= assembly.fragments.size) return
        if (assembly.fragments[fragIndex] != null) return // duplicate

        // Store fragment payload
        val payload = ByteArray(payloadLen)
        System.arraycopy(data, HEADER_SIZE, payload, 0, payloadLen)
        assembly.fragments[fragIndex] = payload
        assembly.receivedCount++

        if (assembly.receivedCount == assembly.fragmentCount) {
            // Frame complete — concatenate fragments
            val frame = ByteArray(assembly.frameSize)
            var offset = 0
            for (frag in assembly.fragments) {
                if (frag != null) {
                    System.arraycopy(frag, 0, frame, offset, frag.size)
                    offset += frag.size
                }
            }

            // Detect skipped frames and only escalate sustained loss bursts.
            if (frameIdLong > lastCompletedFrameId + 1 && lastCompletedFrameId > 0) {
                val skipped = frameIdLong - lastCompletedFrameId - 1
                Log.w(TAG, "[udp] Lost $skipped frame(s) between $lastCompletedFrameId and $frameIdLong")
                totalLost += skipped
                val now = System.nanoTime()
                if (skipped >= KEYFRAME_MIN_SKIPPED_FRAMES &&
                    now - lastKeyframeRequestNs > KEYFRAME_MIN_INTERVAL_NS) {
                    lastKeyframeRequestNs = now
                    onFrameLost()
                }
            }

            lastCompletedFrameId = frameIdLong
            totalFrames++

            // Purge old assemblies
            assemblies.keys.removeAll { it <= frameIdLong }

            onFrameComplete(frame)
        }

        // Purge stale assemblies (older than 100ms)
        val now = System.nanoTime()
        assemblies.entries.removeAll { (_, a) -> now - a.createdAt > STALE_TIMEOUT_NS }
    }

    fun stats(): String = "frames=$totalFrames lost=$totalLost pending=${assemblies.size}"
}
