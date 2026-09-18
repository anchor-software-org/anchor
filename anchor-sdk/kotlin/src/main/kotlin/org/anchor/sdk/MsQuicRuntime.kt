package org.anchor.sdk

/** Loads the two Android-native libraries and exposes a minimal smoke check. */
object MsQuicRuntime {
    init {
        System.loadLibrary("msquic")
        System.loadLibrary("anchor_msquic_jni")
    }

    fun libraryVersion(): String = nativeLibraryVersion()

    private external fun nativeLibraryVersion(): String
}
