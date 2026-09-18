package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.input.InputKey
import org.anchor.sdk.v1.capabilities.input.InputPointerAbsolute
import org.anchor.sdk.v1.capabilities.input.InputPointerRelative
import org.anchor.sdk.v1.capabilities.input.InputPointerButton
import org.anchor.sdk.v1.capabilities.input.InputScroll
import org.anchor.sdk.v1.capabilities.input.InputText

/** Canonical v1 input capability helpers. */
object InputProtocol {
    data class Key(val hidUsage: Int, val pressed: Boolean)
    data class Text(val textUtf8: String)
    data class PointerAbsolute(val x: Int, val y: Int)
    data class PointerRelative(val dx1000Ths: Int, val dy1000Ths: Int)
    data class PointerButton(val buttonValue: Int, val pressed: Boolean)
    data class Scroll(val horizontal120Ths: Int, val vertical120Ths: Int)

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.input"
    const val CAPABILITY_MAJOR = 1
    const val KEY_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputKey"
    const val TEXT_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputText"
    const val POINTER_ABSOLUTE_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerAbsolute"
    const val POINTER_RELATIVE_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerRelative"
    const val POINTER_BUTTON_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerButton"
    const val POINTER_BUTTON_UNSPECIFIED = 0
    const val POINTER_BUTTON_LEFT = 1
    const val POINTER_BUTTON_MIDDLE = 2
    const val POINTER_BUTTON_RIGHT = 3
    const val POINTER_BUTTON_BACK = 4
    const val POINTER_BUTTON_FORWARD = 5
    const val SCROLL_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.input.InputScroll"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME).setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(KEY_TYPE_URL).addRecordTypeUrls(TEXT_TYPE_URL)
        .addRecordTypeUrls(POINTER_ABSOLUTE_TYPE_URL).addRecordTypeUrls(POINTER_RELATIVE_TYPE_URL)
        .addRecordTypeUrls(POINTER_BUTTON_TYPE_URL)
        .addRecordTypeUrls(SCROLL_TYPE_URL).build()

    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder()
        .setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()

    fun encodeKey(hidUsage: Int, pressed: Boolean): ByteArray = InputKey.newBuilder().setHidUsage(hidUsage).setPressed(pressed).build().toByteArray()
    /** USB HID usage for the key names used by the Android touch keyboard. */
    fun hidUsageForKey(key: String): Int? = when {
        key.length == 1 && key[0].lowercaseChar() in 'a'..'z' -> 0x04 + (key[0].lowercaseChar() - 'a')
        key.length == 1 && key[0] in '1'..'9' -> 0x1e + (key[0] - '1')
        key == "0" -> 0x27
        key.equals("Enter", true) || key.equals("Return", true) -> 0x28
        key.equals("Escape", true) || key.equals("Esc", true) -> 0x29
        key.equals("BackSpace", true) || key.equals("Backspace", true) -> 0x2a
        key.equals("Tab", true) -> 0x2b
        key.equals("Space", true) || key == " " -> 0x2c
        key.equals("Delete", true) || key.equals("Del", true) -> 0x4c
        key.equals("Home", true) -> 0x4a
        key.equals("End", true) -> 0x4d
        key.equals("PageUp", true) -> 0x4b
        key.equals("PageDown", true) -> 0x4e
        key.equals("Left", true) -> 0x50
        key.equals("Right", true) -> 0x4f
        key.equals("Down", true) -> 0x51
        key.equals("Up", true) -> 0x52
        key.equals("F1", true) -> 0x3a
        key.equals("F2", true) -> 0x3b
        key.equals("F3", true) -> 0x3c
        key.equals("F4", true) -> 0x3d
        key.equals("F5", true) -> 0x3e
        key.equals("F6", true) -> 0x3f
        key.equals("F7", true) -> 0x40
        key.equals("F8", true) -> 0x41
        key.equals("F9", true) -> 0x42
        key.equals("F10", true) -> 0x43
        key.equals("F11", true) -> 0x44
        key.equals("F12", true) -> 0x45
        key.equals("ctrl", true) || key.equals("control", true) -> 0xe0
        key.equals("shift", true) -> 0xe1
        key.equals("alt", true) -> 0xe2
        key.equals("super", true) || key.equals("meta", true) -> 0xe3
        else -> null
    }
    fun encodeText(text: String): ByteArray = InputText.newBuilder().setTextUtf8(text).build().toByteArray()
    fun encodePointerAbsolute(x: Int, y: Int): ByteArray = InputPointerAbsolute.newBuilder().setX(x.coerceIn(0, 65535)).setY(y.coerceIn(0, 65535)).build().toByteArray()
    fun encodePointerRelative(dx: Float, dy: Float): ByteArray = InputPointerRelative.newBuilder().setDx1000Ths((dx * 1000f).toInt()).setDy1000Ths((dy * 1000f).toInt()).build().toByteArray()
    fun encodePointerButton(buttonValue: Int, pressed: Boolean): ByteArray = InputPointerButton.newBuilder().setButtonValue(buttonValue).setPressed(pressed).build().toByteArray()
    fun encodeScroll(horizontal120ths: Int, vertical120ths: Int): ByteArray = InputScroll.newBuilder().setHorizontal120Ths(horizontal120ths).setVertical120Ths(vertical120ths).build().toByteArray()
    fun decodeKey(bytes: ByteArray): Key = InputKey.parseFrom(bytes)
        .let { Key(it.hidUsage, it.pressed) }
    fun decodeText(bytes: ByteArray): Text = InputText.parseFrom(bytes)
        .let { Text(it.textUtf8) }
    fun decodePointerAbsolute(bytes: ByteArray): PointerAbsolute = InputPointerAbsolute.parseFrom(bytes)
        .let { PointerAbsolute(it.x, it.y) }
    fun decodePointerRelative(bytes: ByteArray): PointerRelative = InputPointerRelative.parseFrom(bytes)
        .let { PointerRelative(it.dx1000Ths, it.dy1000Ths) }
    fun decodePointerButton(bytes: ByteArray): PointerButton = InputPointerButton.parseFrom(bytes)
        .let { PointerButton(it.buttonValue, it.pressed) }
    fun decodeScroll(bytes: ByteArray): Scroll = InputScroll.parseFrom(bytes)
        .let { Scroll(it.horizontal120Ths, it.vertical120Ths) }
}
