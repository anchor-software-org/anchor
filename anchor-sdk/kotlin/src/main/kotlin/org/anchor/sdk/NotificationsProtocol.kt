package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.notifications.NotificationDismissed
import org.anchor.sdk.v1.capabilities.notifications.NotificationInvokeAction
import org.anchor.sdk.v1.capabilities.notifications.NotificationPosted

/** Canonical v1 notification capability names and protobuf helpers. */
object NotificationsProtocol {
    data class Action(val id: String, val label: String)

    data class Posted(
        val notificationId: String,
        val applicationId: String,
        val applicationName: String,
        val title: String,
        val body: String,
        val postedAtUnixMs: Long,
        val actions: List<Action>,
    )

    data class Dismissed(val notificationId: String)
    data class InvokeAction(val notificationId: String, val actionId: String)

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.notifications"
    const val CAPABILITY_MAJOR = 1
    const val POSTED_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationPosted"
    const val DISMISSED_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationDismissed"
    const val INVOKE_ACTION_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationInvokeAction"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME)
        .setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(POSTED_TYPE_URL)
        .addRecordTypeUrls(DISMISSED_TYPE_URL)
        .addRecordTypeUrls(INVOKE_ACTION_TYPE_URL)
        .build()

    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder()
        .setEndpointId(ENDPOINT_ID)
        .addCapabilities(advertisement())
        .build()

    fun encodePosted(
        notificationId: String,
        applicationId: String,
        applicationName: String,
        title: String,
        body: String,
        postedAtUnixMs: Long,
    ): ByteArray = NotificationPosted.newBuilder()
        .setNotificationId(notificationId)
        .setApplicationId(applicationId)
        .setApplicationName(applicationName)
        .setTitle(title)
        .setBody(body)
        .setPostedAtUnixMs(postedAtUnixMs)
        .build()
        .toByteArray()
    fun decodePosted(bytes: ByteArray): Posted = NotificationPosted.parseFrom(bytes).let {
        Posted(
            notificationId = it.notificationId,
            applicationId = it.applicationId,
            applicationName = it.applicationName,
            title = it.title,
            body = it.body,
            postedAtUnixMs = it.postedAtUnixMs,
            actions = it.actionsList.map { action -> Action(action.actionId, action.label) },
        )
    }
    fun decodeDismissed(bytes: ByteArray): Dismissed = NotificationDismissed.parseFrom(bytes)
        .let { Dismissed(it.notificationId) }
    fun decodeInvokeAction(bytes: ByteArray): InvokeAction = NotificationInvokeAction.parseFrom(bytes)
        .let { InvokeAction(it.notificationId, it.actionId) }
}
