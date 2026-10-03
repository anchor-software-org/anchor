# The native transport uses name-based JNI entry points and creates QuicEvent
# subclasses by their fully-qualified names. Keep this private JNI boundary
# stable while allowing R8 to optimize the rest of the application.
-keep class org.anchor.sdk.MsQuicRuntime { *; }
-keep class org.anchor.sdk.MsQuicTransport { *; }
-keep class org.anchor.sdk.QuicEvent { *; }
-keep class org.anchor.sdk.QuicEvent$* { *; }

# GeneratedMessageLite schemas refer to their generated fields by name. Keep
# the Protocol v1 message surface stable while R8 optimizes the rest of the app.
-keep class org.anchor.sdk.v1.** { *; }
