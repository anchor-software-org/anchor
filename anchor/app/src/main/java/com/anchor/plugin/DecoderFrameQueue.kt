package com.anchor.plugin

/** A dropped encoded reference invalidates queued and future predictive frames. */
internal class DecoderFrameQueue<T>(private val capacity: Int, private val isIdr: (T) -> Boolean) {
    private val frames = ArrayDeque<T>()
    private var needsIdr = true
    val size: Int @Synchronized get() = frames.size

    @Synchronized fun offer(frame: T): Boolean {
        if (frames.size == capacity) clear()
        if (needsIdr && !isIdr(frame)) return false
        needsIdr = false
        frames.addLast(frame)
        return true
    }

    @Synchronized fun poll(): T? = frames.removeFirstOrNull()

    @Synchronized fun clear() {
        frames.clear()
        needsIdr = true
    }
}
