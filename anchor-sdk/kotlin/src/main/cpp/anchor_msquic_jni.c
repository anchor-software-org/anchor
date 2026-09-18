#include <jni.h>
#include <android/log.h>
#include <netdb.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "msquic.h"

#define ANCHOR_ALPN "anchor/1"
#define ANCHOR_EVENT_CONNECTED 0x1u
#define ANCHOR_EVENT_FAILED 0x2u
#define ANCHOR_EVENT_CLOSED 0x4u
#define ANCHOR_LOG_TAG "AnchorMsQuic"

/*
 * A screen IDR is commonly 150-600 KiB. MsQuic's default per-stream receive
 * window is only 64 KiB, which makes a desktop sender stop repeatedly while
 * the receiver advertises more credit. That is reasonable for RPC traffic,
 * but it turns one IDR into seconds of head-of-line blocking on a local Wi-Fi
 * link. This is the receive window advertised by Android to its peer, not an
 * application queue: received bytes are still copied into, and promptly
 * consumed from, the JNI event queue below.
 *
 * The window is also the sender's latency bound. A QUIC stream write completes
 * as soon as the bytes fit in the peer's credit, so unconsumed window is
 * standing queue on a live video stream: 4 MiB is ~600 ms of buffered frames
 * on a 55 Mbps link, and the sender cannot drop the stale frames sitting in
 * it. Size the window for a couple of large access units instead. It stays far
 * above the path's bandwidth-delay product (~35 KiB at 55 Mbps and 5 ms), so
 * throughput is unaffected, while a saturated path now blocks the desktop
 * writer and lets its replaceable-frame queue discard old screen content.
 */
#define ANCHOR_STREAM_RECEIVE_WINDOW (512u * 1024u)

typedef struct anchor_connection_s {
    const QUIC_API_TABLE *api;
    HQUIC registration;
    HQUIC configuration;
    HQUIC connection;
    atomic_uint pending_events;
    atomic_ullong close_code;
    pthread_mutex_t lock;
    pthread_cond_t shutdown_complete;
    int shutdown_finished;
    struct anchor_stream_s *streams;
    struct anchor_event_s *event_head;
    struct anchor_event_s *event_tail;
    /* All queue accounting is protected by `lock`.  These counters describe
     * the JNI handoff queue, not MsQuic's internal buffers.  In particular,
     * they must never be used as a reason to discard reliable stream data. */
    uint64_t queued_event_count;
    uint64_t queued_event_bytes;
    uint64_t queued_event_high_water_count;
    uint64_t queued_event_high_water_bytes;
    uint64_t enqueued_event_count;
    uint64_t enqueued_event_bytes;
    uint64_t dequeued_event_count;
    uint64_t dequeued_event_bytes;
    uint64_t poll_count;
    uint64_t poll_event_count;
    uint64_t poll_byte_count;
    char error[128];
} anchor_connection_t;

typedef struct anchor_stream_s {
    anchor_connection_t *owner;
    HQUIC handle;
    uint64_t id;
    int started;
    QUIC_STATUS start_status;
    struct anchor_stream_s *next;
} anchor_stream_t;

typedef enum anchor_event_kind_e {
    ANCHOR_EVENT_STREAM_DATA,
    ANCHOR_EVENT_DATAGRAM,
} anchor_event_kind_t;

typedef struct anchor_event_s {
    anchor_event_kind_t kind;
    uint64_t stream_id;
    uint8_t *bytes;
    uint32_t length;
    int finished;
    struct anchor_event_s *next;
} anchor_event_t;

typedef struct anchor_send_s {
    QUIC_BUFFER buffer;
    uint8_t bytes[];
} anchor_send_t;

static void anchor_push_payload(anchor_connection_t *anchor, anchor_event_kind_t kind,
    uint64_t stream_id, const QUIC_BUFFER *buffers, uint32_t buffer_count,
    QUIC_RECEIVE_FLAGS flags);
static void anchor_set_error(anchor_connection_t *anchor, const char *message, QUIC_STATUS status);
static void anchor_wait_for_shutdown(anchor_connection_t *anchor);
static QUIC_STATUS QUIC_API anchor_stream_callback(HQUIC stream, void *context,
    QUIC_STREAM_EVENT *event);

static void anchor_push_event(anchor_connection_t *anchor, anchor_event_t *event) {
    pthread_mutex_lock(&anchor->lock);
    if (anchor->event_tail == NULL) {
        anchor->event_head = event;
    } else {
        anchor->event_tail->next = event;
    }
    anchor->event_tail = event;
    anchor->queued_event_count++;
    anchor->queued_event_bytes += event->length;
    if (anchor->queued_event_count > anchor->queued_event_high_water_count) {
        anchor->queued_event_high_water_count = anchor->queued_event_count;
    }
    if (anchor->queued_event_bytes > anchor->queued_event_high_water_bytes) {
        anchor->queued_event_high_water_bytes = anchor->queued_event_bytes;
    }
    anchor->enqueued_event_count++;
    anchor->enqueued_event_bytes += event->length;
    pthread_mutex_unlock(&anchor->lock);
}

static void anchor_push_payload(anchor_connection_t *anchor, anchor_event_kind_t kind,
    uint64_t stream_id, const QUIC_BUFFER *buffers, uint32_t buffer_count,
    QUIC_RECEIVE_FLAGS flags) {
    size_t total = 0;
    int too_large = 0;
    for (uint32_t i = 0; i < buffer_count; i++) {
        if (buffers[i].Length > UINT32_MAX - total) {
            too_large = 1;
            break;
        }
        total += buffers[i].Length;
    }
    if (too_large || total > UINT32_MAX) {
        anchor_set_error(anchor, "received payload is too large", QUIC_STATUS_BUFFER_TOO_SMALL);
        return;
    }
    anchor_event_t *event = calloc(1, sizeof(*event));
    if (event == NULL || (total != 0 && (event->bytes = malloc(total)) == NULL)) {
        free(event);
        anchor_set_error(anchor, "could not allocate received payload", QUIC_STATUS_OUT_OF_MEMORY);
        return;
    }
    event->kind = kind;
    event->stream_id = stream_id;
    event->length = (uint32_t)total;
    event->finished = (flags & QUIC_RECEIVE_FLAG_FIN) != 0;
    size_t offset = 0;
    if (total != 0) {
        for (uint32_t i = 0; i < buffer_count; i++) {
            memcpy(event->bytes + offset, buffers[i].Buffer, buffers[i].Length);
            offset += buffers[i].Length;
        }
    }
    anchor_push_event(anchor, event);
}

static void anchor_set_error(anchor_connection_t *anchor, const char *message, QUIC_STATUS status) {
    snprintf(anchor->error, sizeof(anchor->error), "%s: 0x%08x", message, (unsigned int)status);
    __android_log_print(ANDROID_LOG_ERROR, ANCHOR_LOG_TAG, "%s", anchor->error);
    atomic_fetch_or(&anchor->pending_events, ANCHOR_EVENT_FAILED);
}

/*
 * MsQuic invokes SHUTDOWN_COMPLETE asynchronously.  Do not use a timeout
 * here: destroying `lock` while that callback is still possible turns a
 * recoverable connection failure into a process-aborting use-after-free.
 */
static void anchor_wait_for_shutdown(anchor_connection_t *anchor) {
    pthread_mutex_lock(&anchor->lock);
    while (!anchor->shutdown_finished) {
        (void)pthread_cond_wait(&anchor->shutdown_complete, &anchor->lock);
    }
    pthread_mutex_unlock(&anchor->lock);
}

static QUIC_STATUS QUIC_API anchor_connection_callback(
    HQUIC connection, void *context, QUIC_CONNECTION_EVENT *event) {
    anchor_connection_t *anchor = context;
    switch (event->Type) {
    case QUIC_CONNECTION_EVENT_CONNECTED:
        __android_log_print(ANDROID_LOG_INFO, ANCHOR_LOG_TAG, "connected; negotiated ALPN length=%u", event->CONNECTED.NegotiatedAlpnLength);
        if (event->CONNECTED.NegotiatedAlpnLength != strlen(ANCHOR_ALPN) ||
            memcmp(event->CONNECTED.NegotiatedAlpn, ANCHOR_ALPN, strlen(ANCHOR_ALPN)) != 0) {
            anchor_set_error(anchor, "peer negotiated an unexpected ALPN", QUIC_STATUS_ALPN_NEG_FAILURE);
            anchor->api->ConnectionShutdown(connection, QUIC_CONNECTION_SHUTDOWN_FLAG_NONE, 0);
        } else {
            atomic_fetch_or(&anchor->pending_events, ANCHOR_EVENT_CONNECTED);
        }
        break;
    case QUIC_CONNECTION_EVENT_PEER_STREAM_STARTED: {
        anchor_stream_t *stream = calloc(1, sizeof(*stream));
        if (stream == NULL) {
            anchor->api->StreamShutdown(event->PEER_STREAM_STARTED.Stream,
                QUIC_STREAM_SHUTDOWN_FLAG_ABORT, 0);
            break;
        }
        stream->owner = anchor;
        stream->handle = event->PEER_STREAM_STARTED.Stream;
        uint32_t id_length = sizeof(stream->id);
        if (QUIC_FAILED(anchor->api->GetParam(stream->handle, QUIC_PARAM_STREAM_ID,
                &id_length, &stream->id))) {
            free(stream);
            anchor->api->StreamShutdown(event->PEER_STREAM_STARTED.Stream,
                QUIC_STREAM_SHUTDOWN_FLAG_ABORT, 0);
            break;
        }
        anchor->api->SetCallbackHandler(stream->handle, (void *)anchor_stream_callback, stream);
        pthread_mutex_lock(&anchor->lock);
        stream->next = anchor->streams;
        anchor->streams = stream;
        pthread_mutex_unlock(&anchor->lock);
        break;
    }
    case QUIC_CONNECTION_EVENT_DATAGRAM_RECEIVED:
        anchor_push_payload(anchor, ANCHOR_EVENT_DATAGRAM, 0,
            event->DATAGRAM_RECEIVED.Buffer, 1, event->DATAGRAM_RECEIVED.Flags);
        break;
    case QUIC_CONNECTION_EVENT_DATAGRAM_SEND_STATE_CHANGED:
        if (event->DATAGRAM_SEND_STATE_CHANGED.State >= QUIC_DATAGRAM_SEND_LOST_DISCARDED) {
            free(event->DATAGRAM_SEND_STATE_CHANGED.ClientContext);
        }
        break;
    case QUIC_CONNECTION_EVENT_SHUTDOWN_COMPLETE:
        pthread_mutex_lock(&anchor->lock);
        anchor->shutdown_finished = 1;
        pthread_cond_broadcast(&anchor->shutdown_complete);
        pthread_mutex_unlock(&anchor->lock);
        break;
    case QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT:
        __android_log_print(ANDROID_LOG_ERROR, ANCHOR_LOG_TAG, "transport shutdown status=0x%08x error_code=%llu",
            (unsigned int)event->SHUTDOWN_INITIATED_BY_TRANSPORT.Status,
            (unsigned long long)event->SHUTDOWN_INITIATED_BY_TRANSPORT.ErrorCode);
        atomic_store(&anchor->close_code, event->SHUTDOWN_INITIATED_BY_TRANSPORT.ErrorCode);
        anchor_set_error(anchor, "QUIC transport shutdown", event->SHUTDOWN_INITIATED_BY_TRANSPORT.Status);
        break;
    case QUIC_CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER:
        __android_log_print(ANDROID_LOG_WARN, ANCHOR_LOG_TAG, "peer shutdown error_code=%llu",
            (unsigned long long)event->SHUTDOWN_INITIATED_BY_PEER.ErrorCode);
        atomic_store(&anchor->close_code, event->SHUTDOWN_INITIATED_BY_PEER.ErrorCode);
        atomic_fetch_or(&anchor->pending_events, ANCHOR_EVENT_CLOSED);
        break;
    default:
        break;
    }
    return QUIC_STATUS_SUCCESS;
}

static QUIC_STATUS QUIC_API anchor_stream_callback(
    HQUIC stream_handle, void *context, QUIC_STREAM_EVENT *event) {
    anchor_stream_t *stream = context;
    anchor_connection_t *anchor = stream->owner;
    switch (event->Type) {
    case QUIC_STREAM_EVENT_START_COMPLETE:
        pthread_mutex_lock(&anchor->lock);
        stream->id = event->START_COMPLETE.ID;
        stream->start_status = event->START_COMPLETE.Status;
        stream->started = 1;
        pthread_cond_broadcast(&anchor->shutdown_complete);
        pthread_mutex_unlock(&anchor->lock);
        break;
    case QUIC_STREAM_EVENT_RECEIVE:
        anchor_push_payload(anchor, ANCHOR_EVENT_STREAM_DATA, stream->id,
            event->RECEIVE.Buffers, event->RECEIVE.BufferCount, event->RECEIVE.Flags);
        // Returning SUCCESS consumes the receive buffers. Calling
        // StreamReceiveComplete as well would acknowledge the same bytes a
        // second time and can stall subsequent data on this stream.
        break;
    case QUIC_STREAM_EVENT_SEND_COMPLETE:
        free(event->SEND_COMPLETE.ClientContext);
        break;
    case QUIC_STREAM_EVENT_SHUTDOWN_COMPLETE:
        anchor->api->StreamClose(stream_handle);
        pthread_mutex_lock(&anchor->lock);
        anchor_stream_t **cursor = &anchor->streams;
        while (*cursor != NULL && *cursor != stream) cursor = &(*cursor)->next;
        if (*cursor == stream) *cursor = stream->next;
        pthread_mutex_unlock(&anchor->lock);
        free(stream);
        break;
    default:
        break;
    }
    return QUIC_STATUS_SUCCESS;
}

static char *anchor_copy_java_string(JNIEnv *env, jstring value) {
    const char *chars = (*env)->GetStringUTFChars(env, value, NULL);
    if (chars == NULL) return NULL;
    char *copy = strdup(chars);
    (*env)->ReleaseStringUTFChars(env, value, chars);
    return copy;
}

static void anchor_throw_unsupported(JNIEnv *env, const char *message) {
    jclass exception = (*env)->FindClass(env, "java/lang/UnsupportedOperationException");
    if (exception != NULL) (*env)->ThrowNew(env, exception, message);
}

JNIEXPORT jstring JNICALL
Java_org_anchor_sdk_MsQuicRuntime_nativeLibraryVersion(JNIEnv *env, jclass clazz) {
    (void)clazz;
    const QUIC_API_TABLE *api = NULL;
    QUIC_STATUS status = MsQuicOpen2(&api);
    if (QUIC_FAILED(status) || api == NULL) return (*env)->NewStringUTF(env, "MsQuicOpen2 failed");

    uint32_t version[4] = {0};
    uint32_t version_length = sizeof(version);
    status = api->GetParam(NULL, QUIC_PARAM_GLOBAL_LIBRARY_VERSION, &version_length, version);
    MsQuicClose(api);
    if (QUIC_FAILED(status) || version_length != sizeof(version)) return (*env)->NewStringUTF(env, "MsQuic version query failed");

    char result[32];
    snprintf(result, sizeof(result), "%u.%u.%u.%u", version[0], version[1], version[2], version[3]);
    return (*env)->NewStringUTF(env, result);
}

JNIEXPORT jlong JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeCreate(
    JNIEnv *env, jobject instance, jstring host, jint port, jstring server_name,
    jbyteArray expected_fingerprint, jstring certificate_path, jstring private_key_path,
    jstring trusted_certificate_path, jboolean pairing_bootstrap) {
    (void)instance;
    if ((!pairing_bootstrap && (*env)->GetArrayLength(env, expected_fingerprint) != 32) ||
        (pairing_bootstrap && (*env)->GetArrayLength(env, expected_fingerprint) != 0)) return 0;

    anchor_connection_t *anchor = NULL;
    QUIC_STATUS status = QUIC_STATUS_SUCCESS;
    const char *failed_step = "argument conversion";
    char *native_host = anchor_copy_java_string(env, host);

    char *native_server_name = anchor_copy_java_string(env, server_name);
    char *native_certificate_path = anchor_copy_java_string(env, certificate_path);
    char *native_private_key_path = anchor_copy_java_string(env, private_key_path);
    char *native_trusted_certificate_path = anchor_copy_java_string(env, trusted_certificate_path);
    if (native_host == NULL || native_server_name == NULL || native_certificate_path == NULL ||
        native_private_key_path == NULL || native_trusted_certificate_path == NULL) goto error;

    anchor = calloc(1, sizeof(*anchor));
    if (anchor == NULL) goto error;
    if (pthread_mutex_init(&anchor->lock, NULL) != 0) {
        free(anchor);
        anchor = NULL;
        goto error;
    }
    if (pthread_cond_init(&anchor->shutdown_complete, NULL) != 0) {
        pthread_mutex_destroy(&anchor->lock);
        free(anchor);
        anchor = NULL;
        goto error;
    }
    failed_step = "MsQuicOpen2";
    status = MsQuicOpen2(&anchor->api);
    if (QUIC_FAILED(status)) goto error;

    QUIC_REGISTRATION_CONFIG registration_config = {"anchor-sdk", QUIC_EXECUTION_PROFILE_LOW_LATENCY};
    failed_step = "RegistrationOpen";
    if (QUIC_FAILED(status = anchor->api->RegistrationOpen(&registration_config, &anchor->registration))) goto error;

    uint8_t alpn_bytes[] = ANCHOR_ALPN;
    QUIC_BUFFER alpn = {sizeof(ANCHOR_ALPN) - 1, alpn_bytes};
    QUIC_SETTINGS settings = {0};
    // Match the Rust/Quinn adapter: an idle control session is valid, so use
    // periodic QUIC keepalives instead of allowing the default short timeout
    // to tear down a healthy paired connection.
    settings.IdleTimeoutMs = 120000;
    settings.IsSet.IdleTimeoutMs = TRUE;
    settings.KeepAliveIntervalMs = 10000;
    settings.IsSet.KeepAliveIntervalMs = TRUE;
    settings.DatagramReceiveEnabled = TRUE;
    settings.IsSet.DatagramReceiveEnabled = TRUE;
    settings.PeerBidiStreamCount = 16;
    settings.IsSet.PeerBidiStreamCount = TRUE;
    settings.StreamRecvWindowBidiLocalDefault = ANCHOR_STREAM_RECEIVE_WINDOW;
    settings.IsSet.StreamRecvWindowBidiLocalDefault = TRUE;
    settings.StreamRecvWindowBidiRemoteDefault = ANCHOR_STREAM_RECEIVE_WINDOW;
    settings.IsSet.StreamRecvWindowBidiRemoteDefault = TRUE;
    failed_step = "ConfigurationOpen";
    if (QUIC_FAILED(status = anchor->api->ConfigurationOpen(
            anchor->registration, &alpn, 1, &settings, sizeof(settings), NULL, &anchor->configuration))) goto error;

    QUIC_CERTIFICATE_FILE certificate = {native_private_key_path, native_certificate_path};
    QUIC_CREDENTIAL_CONFIG credentials = {0};
    credentials.Type = QUIC_CREDENTIAL_TYPE_CERTIFICATE_FILE;
    credentials.Flags = QUIC_CREDENTIAL_FLAG_CLIENT;
    if (pairing_bootstrap) {
        // Direct-address enrollment is deliberately the sole exception to
        // certificate pinning. The Kotlin pairing API exposes no capability
        // or normal-session surface on this connection; a desktop approval
        // returns the pin used by every subsequent connection.
        credentials.Flags |= QUIC_CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION;
    } else {
        credentials.Flags |= QUIC_CREDENTIAL_FLAG_USE_TLS_BUILTIN_CERTIFICATE_VALIDATION |
            QUIC_CREDENTIAL_FLAG_SET_CA_CERTIFICATE_FILE;
    }
    credentials.CertificateFile = &certificate;
    credentials.CaCertificateFile = pairing_bootstrap ? NULL : native_trusted_certificate_path;
    failed_step = "ConfigurationLoadCredential";
    if (QUIC_FAILED(status = anchor->api->ConfigurationLoadCredential(anchor->configuration, &credentials))) goto error;
    failed_step = "ConnectionOpen";
    if (QUIC_FAILED(status = anchor->api->ConnectionOpen(
            anchor->registration, anchor_connection_callback, anchor, &anchor->connection))) goto error;

    char port_string[6];
    snprintf(port_string, sizeof(port_string), "%d", port);
    struct addrinfo hints = {0};
    hints.ai_socktype = SOCK_DGRAM;
    hints.ai_family = AF_UNSPEC;
    struct addrinfo *resolved = NULL;
    failed_step = "getaddrinfo";
    int address_status = getaddrinfo(native_host, port_string, &hints, &resolved);
    if (address_status != 0 || resolved == NULL ||
        resolved->ai_addrlen > sizeof(QUIC_ADDR)) {
        __android_log_print(ANDROID_LOG_ERROR, ANCHOR_LOG_TAG,
            "nativeCreate getaddrinfo failed: host=%s port=%s gai=%d (%s)",
            native_host, port_string, address_status,
            address_status == 0 ? "no address" : gai_strerror(address_status));
        if (resolved != NULL) freeaddrinfo(resolved);
        goto error;
    }
    QUIC_ADDR remote_address = {0};
    memcpy(&remote_address, resolved->ai_addr, resolved->ai_addrlen);
    freeaddrinfo(resolved);
    failed_step = "SetParam(remote address)";
    if (QUIC_FAILED(status = anchor->api->SetParam(
            anchor->connection, QUIC_PARAM_CONN_REMOTE_ADDRESS, sizeof(remote_address), &remote_address))) goto error;
    failed_step = "ConnectionStart";
    if (QUIC_FAILED(status = anchor->api->ConnectionStart(
            anchor->connection, anchor->configuration, QUIC_ADDRESS_FAMILY_UNSPEC,
            native_server_name, (uint16_t)port))) goto error;

    free(native_host);
    free(native_server_name);
    free(native_certificate_path);
    free(native_private_key_path);
    free(native_trusted_certificate_path);
    return (jlong)(uintptr_t)anchor;

error:
    __android_log_print(ANDROID_LOG_ERROR, ANCHOR_LOG_TAG,
        "nativeCreate failed at %s: status=0x%08x host=%s port=%d server_name=%s",
        failed_step, (unsigned int)status,
        native_host != NULL ? native_host : "<null>", (int)port,
        native_server_name != NULL ? native_server_name : "<null>");
    free(native_host);
    free(native_server_name);
    free(native_certificate_path);
    free(native_private_key_path);
    free(native_trusted_certificate_path);
    if (anchor != NULL) {
        if (anchor->connection != NULL) {
            anchor->api->ConnectionShutdown(anchor->connection, QUIC_CONNECTION_SHUTDOWN_FLAG_SILENT, 0);
            anchor_wait_for_shutdown(anchor);
            anchor->api->ConnectionClose(anchor->connection);
        }
        if (anchor->configuration != NULL) anchor->api->ConfigurationClose(anchor->configuration);
        if (anchor->registration != NULL) anchor->api->RegistrationClose(anchor->registration);
        if (anchor->api != NULL) MsQuicClose(anchor->api);
        pthread_cond_destroy(&anchor->shutdown_complete);
        pthread_mutex_destroy(&anchor->lock);
        free(anchor);
    }
    return 0;
}

JNIEXPORT jobject JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativePoll(JNIEnv *env, jobject instance, jlong handle) {
    (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    jclass list_class = (*env)->FindClass(env, "java/util/ArrayList");
    jmethodID list_constructor = (*env)->GetMethodID(env, list_class, "<init>", "()V");
    jmethodID list_add = (*env)->GetMethodID(env, list_class, "add", "(Ljava/lang/Object;)Z");
    jobject events = (*env)->NewObject(env, list_class, list_constructor);
    uint32_t pending = atomic_exchange(&anchor->pending_events, 0);
    uint64_t poll_events = 0;
    uint64_t poll_bytes = 0;
    pthread_mutex_lock(&anchor->lock);
    anchor->poll_count++;
    pthread_mutex_unlock(&anchor->lock);
    if (pending & ANCHOR_EVENT_CONNECTED) {
        jclass connected = (*env)->FindClass(env, "org/anchor/sdk/QuicEvent$Connected");
        jfieldID singleton = (*env)->GetStaticFieldID(env, connected, "INSTANCE", "Lorg/anchor/sdk/QuicEvent$Connected;");
        (*env)->CallBooleanMethod(env, events, list_add, (*env)->GetStaticObjectField(env, connected, singleton));
    }
    if (pending & ANCHOR_EVENT_FAILED) {
        jclass failed = (*env)->FindClass(env, "org/anchor/sdk/QuicEvent$Failed");
        jmethodID constructor = (*env)->GetMethodID(env, failed, "<init>", "(Ljava/lang/String;)V");
        jobject event = (*env)->NewObject(env, failed, constructor, (*env)->NewStringUTF(env, anchor->error));
        (*env)->CallBooleanMethod(env, events, list_add, event);
    }
    for (;;) {
        pthread_mutex_lock(&anchor->lock);
        anchor_event_t *event = anchor->event_head;
        if (event != NULL) {
            anchor->event_head = event->next;
            if (anchor->event_head == NULL) anchor->event_tail = NULL;
            anchor->queued_event_count--;
            anchor->queued_event_bytes -= event->length;
            anchor->dequeued_event_count++;
            anchor->dequeued_event_bytes += event->length;
        }
        pthread_mutex_unlock(&anchor->lock);
        if (event == NULL) break;

        poll_events++;
        poll_bytes += event->length;

        if (event->kind == ANCHOR_EVENT_STREAM_DATA) {
            jclass stream_data = (*env)->FindClass(env, "org/anchor/sdk/QuicEvent$StreamData");
            jmethodID constructor = (*env)->GetMethodID(env, stream_data, "<init>", "(J[BZ)V");
            jbyteArray bytes = (*env)->NewByteArray(env, (jsize)event->length);
            if (event->length != 0) {
                (*env)->SetByteArrayRegion(env, bytes, 0, (jsize)event->length,
                    (const jbyte *)event->bytes);
            }
            jobject java_event = (*env)->NewObject(env, stream_data, constructor,
                (jlong)event->stream_id, bytes, (jboolean)event->finished);
            (*env)->CallBooleanMethod(env, events, list_add, java_event);
        } else {
            jclass datagram = (*env)->FindClass(env, "org/anchor/sdk/QuicEvent$Datagram");
            jmethodID constructor = (*env)->GetMethodID(env, datagram, "<init>", "([B)V");
            jbyteArray bytes = (*env)->NewByteArray(env, (jsize)event->length);
            if (event->length != 0) {
                (*env)->SetByteArrayRegion(env, bytes, 0, (jsize)event->length,
                    (const jbyte *)event->bytes);
            }
            jobject java_event = (*env)->NewObject(env, datagram, constructor, bytes);
            (*env)->CallBooleanMethod(env, events, list_add, java_event);
        }
        free(event->bytes);
        free(event);
    }
    pthread_mutex_lock(&anchor->lock);
    anchor->poll_event_count += poll_events;
    anchor->poll_byte_count += poll_bytes;
    pthread_mutex_unlock(&anchor->lock);
    // Deliver queued stream/datagram payloads before the connection-close
    // notification. A peer may close immediately after its final payload.
    if (pending & ANCHOR_EVENT_CLOSED) {
        jclass closed = (*env)->FindClass(env, "org/anchor/sdk/QuicEvent$Closed");
        jmethodID constructor = (*env)->GetMethodID(env, closed, "<init>", "(JLjava/lang/String;)V");
        jobject event = (*env)->NewObject(env, closed, constructor,
            (jlong)atomic_load(&anchor->close_code), (*env)->NewStringUTF(env, "closed by peer"));
        (*env)->CallBooleanMethod(env, events, list_add, event);
    }
    return events;
}

/*
 * Return a stable, allocation-free snapshot of the JNI queue counters.  The
 * order is part of the private JNI ABI and is mirrored by QuicTransportMetrics
 * in Kotlin.  Snapshotting is deliberately separate from nativePoll: a
 * diagnostic reader may inspect the queue without draining reliable bytes.
 */
JNIEXPORT jlongArray JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeMetrics(JNIEnv *env, jobject instance, jlong handle) {
    (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    jlongArray result = (*env)->NewLongArray(env, 11);
    if (result == NULL || anchor == NULL) return result;
    jlong values[11];
    pthread_mutex_lock(&anchor->lock);
    values[0] = (jlong)anchor->queued_event_count;
    values[1] = (jlong)anchor->queued_event_bytes;
    values[2] = (jlong)anchor->queued_event_high_water_count;
    values[3] = (jlong)anchor->queued_event_high_water_bytes;
    values[4] = (jlong)anchor->enqueued_event_count;
    values[5] = (jlong)anchor->enqueued_event_bytes;
    values[6] = (jlong)anchor->dequeued_event_count;
    values[7] = (jlong)anchor->dequeued_event_bytes;
    values[8] = (jlong)anchor->poll_count;
    values[9] = (jlong)anchor->poll_event_count;
    values[10] = (jlong)anchor->poll_byte_count;
    pthread_mutex_unlock(&anchor->lock);
    (*env)->SetLongArrayRegion(env, result, 0, 11, values);
    return result;
}

static anchor_stream_t *anchor_find_stream_locked(anchor_connection_t *anchor, uint64_t id) {
    anchor_stream_t *stream = anchor->streams;
    while (stream != NULL && stream->id != id) stream = stream->next;
    return stream;
}

JNIEXPORT jlong JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeOpenBidirectionalStream(JNIEnv *env, jobject instance, jlong handle) {
    (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    if (anchor == NULL) return 0;
    anchor_stream_t *stream = calloc(1, sizeof(*stream));
    if (stream == NULL) {
        anchor_throw_unsupported(env, "could not allocate MsQuic stream");
        return 0;
    }
    stream->owner = anchor;
    QUIC_STATUS status = anchor->api->StreamOpen(anchor->connection,
        QUIC_STREAM_OPEN_FLAG_NONE, (void *)anchor_stream_callback, stream, &stream->handle);
    if (QUIC_FAILED(status)) {
        free(stream);
        anchor_throw_unsupported(env, "MsQuic could not open a bidirectional stream");
        return 0;
    }
    pthread_mutex_lock(&anchor->lock);
    stream->next = anchor->streams;
    anchor->streams = stream;
    pthread_mutex_unlock(&anchor->lock);
    status = anchor->api->StreamStart(stream->handle, QUIC_STREAM_START_FLAG_IMMEDIATE);
    if (QUIC_FAILED(status)) {
        anchor->api->StreamShutdown(stream->handle, QUIC_STREAM_SHUTDOWN_FLAG_ABORT, 0);
        anchor_throw_unsupported(env, "MsQuic could not start a bidirectional stream");
        return 0;
    }
    pthread_mutex_lock(&anchor->lock);
    if (!stream->started) {
        struct timespec deadline;
        clock_gettime(CLOCK_REALTIME, &deadline);
        deadline.tv_sec += 2;
        while (!stream->started) {
            int wait_status = pthread_cond_timedwait(&anchor->shutdown_complete, &anchor->lock, &deadline);
            if (wait_status != 0) break;
        }
    }
    int started = stream->started;
    uint64_t stream_id = stream->id;
    QUIC_STATUS start_status = stream->start_status;
    pthread_mutex_unlock(&anchor->lock);
    if (!started || QUIC_FAILED(start_status)) {
        anchor->api->StreamShutdown(stream->handle, QUIC_STREAM_SHUTDOWN_FLAG_ABORT, 0);
        anchor_throw_unsupported(env, "MsQuic stream start did not complete");
        return 0;
    }
    return (jlong)stream_id;
}

JNIEXPORT void JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeSendStream(
    JNIEnv *env, jobject instance, jlong handle, jlong stream_id, jbyteArray bytes, jboolean finish) {
    (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    anchor_stream_t *stream = NULL;
    if (anchor != NULL) {
        pthread_mutex_lock(&anchor->lock);
        stream = anchor_find_stream_locked(anchor, (uint64_t)stream_id);
        pthread_mutex_unlock(&anchor->lock);
    }
    if (stream == NULL) {
        anchor_throw_unsupported(env, "unknown MsQuic stream ID");
        return;
    }
    jsize length = (*env)->GetArrayLength(env, bytes);
    anchor_send_t *send = NULL;
    if (length != 0) {
        send = malloc(sizeof(*send) + (size_t)length);
        if (send == NULL) {
            anchor_throw_unsupported(env, "could not allocate MsQuic stream send buffer");
            return;
        }
        send->buffer.Length = (uint32_t)length;
        send->buffer.Buffer = send->bytes;
        (*env)->GetByteArrayRegion(env, bytes, 0, length, (jbyte *)send->bytes);
    }
    QUIC_SEND_FLAGS flags = QUIC_SEND_FLAG_NONE;
    if (finish) flags |= QUIC_SEND_FLAG_FIN;
    // Keep the stream entry alive through StreamSend. Shutdown callbacks may
    // unlink and free it as soon as the connection is closing.
    pthread_mutex_lock(&anchor->lock);
    stream = anchor_find_stream_locked(anchor, (uint64_t)stream_id);
    if (stream == NULL) {
        pthread_mutex_unlock(&anchor->lock);
        free(send);
        anchor_throw_unsupported(env, "unknown MsQuic stream ID");
        return;
    }
    QUIC_STATUS status = anchor->api->StreamSend(stream->handle,
        send == NULL ? NULL : &send->buffer, send == NULL ? 0 : 1, flags, send);
    pthread_mutex_unlock(&anchor->lock);
    if (QUIC_FAILED(status)) {
        free(send);
        anchor_throw_unsupported(env, "MsQuic stream send failed");
    } else {
    }
}

JNIEXPORT void JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeSendDatagram(
    JNIEnv *env, jobject instance, jlong handle, jbyteArray bytes) {
    (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    if (anchor == NULL) return;
    jsize length = (*env)->GetArrayLength(env, bytes);
    anchor_send_t *send = malloc(sizeof(*send) + (size_t)length);
    if (send == NULL) {
        anchor_throw_unsupported(env, "could not allocate MsQuic datagram buffer");
        return;
    }
    send->buffer.Length = (uint32_t)length;
    send->buffer.Buffer = send->bytes;
    (*env)->GetByteArrayRegion(env, bytes, 0, length, (jbyte *)send->bytes);
    QUIC_STATUS status = anchor->api->DatagramSend(anchor->connection, &send->buffer, 1,
        QUIC_SEND_FLAG_NONE, send);
    if (QUIC_FAILED(status)) {
        free(send);
        anchor_throw_unsupported(env, "MsQuic datagram send failed");
    }
}

JNIEXPORT void JNICALL
Java_org_anchor_sdk_MsQuicTransport_nativeClose(JNIEnv *env, jobject instance, jlong handle) {
    (void)env; (void)instance;
    anchor_connection_t *anchor = (anchor_connection_t *)(uintptr_t)handle;
    if (anchor == NULL) return;
    if (anchor->connection != NULL) {
        anchor->api->ConnectionShutdown(anchor->connection, QUIC_CONNECTION_SHUTDOWN_FLAG_SILENT, 0);
        anchor_wait_for_shutdown(anchor);
        anchor->api->ConnectionClose(anchor->connection);
    }
    pthread_mutex_lock(&anchor->lock);
    anchor_event_t *event = anchor->event_head;
    anchor->event_head = NULL;
    anchor->event_tail = NULL;
    anchor_stream_t *stream = anchor->streams;
    anchor->streams = NULL;
    pthread_mutex_unlock(&anchor->lock);
    while (event != NULL) {
        anchor_event_t *next = event->next;
        free(event->bytes);
        free(event);
        event = next;
    }
    while (stream != NULL) {
        anchor_stream_t *next = stream->next;
        if (stream->handle != NULL) anchor->api->StreamClose(stream->handle);
        free(stream);
        stream = next;
    }
    if (anchor->configuration != NULL) anchor->api->ConfigurationClose(anchor->configuration);
    if (anchor->registration != NULL) anchor->api->RegistrationClose(anchor->registration);
    if (anchor->api != NULL) MsQuicClose(anchor->api);
    pthread_cond_destroy(&anchor->shutdown_complete);
    pthread_mutex_destroy(&anchor->lock);
    free(anchor);
}
