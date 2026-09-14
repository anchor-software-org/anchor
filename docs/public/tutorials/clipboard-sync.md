# Sync your clipboard

Anchor can sync text and PNG images between a Linux desktop and an Android phone.

Desktop clipboard changes are shared automatically. Android cannot read the clipboard in the background, so sending content from the phone requires a tap.

## Before you start

- Install Anchor on both devices and [pair them](/docs/installation).
- Keep both devices connected.
- Run the desktop app in a supported Wayland session. See [Compatibility](/docs/compatibility).

## Desktop to phone

1. Open the **Clipboard** section in Anchor on your desktop.
2. Copy text or a PNG image in any desktop app.
3. Anchor adds the item to its clipboard history and sends it to the phone.
4. Paste the item in another app on your phone.

Desktop sharing is enabled by default. If it is disabled in your desktop settings, changes are kept locally and are not sent.

## Phone to desktop

1. Open **Clipboard Sync** in the Android app.
2. Copy the text you want to send.
3. Return to Anchor while it is still in the foreground.
4. Tap **Send to Desktop**.
5. Paste the text on your desktop.

Android sends text from the current clipboard. It does not send images to the desktop.

## Clipboard history

Anchor keeps recent clipboard items in the Clipboard screen on each device. The history holds up to 50 items by default.

- On the desktop, select **Copy** on a text item to copy it locally, or select **Send again** to send it to the phone.
- On the phone, tap a history item to copy it to the phone's clipboard.
- Select **Clear all** on the desktop or the trash button on the phone to clear that device's history.

Anchor supports text and PNG images. Items larger than the configured size limit are skipped; the desktop limit is 1 MB by default.

## Troubleshooting

### Nothing arrives on the phone

- Check that the devices are connected.
- Check that desktop clipboard sharing is enabled.
- Confirm that the desktop is running in a supported Wayland session.
- Check the desktop Clipboard section for the new item. Unsupported clipboard formats are not sent.

### Send to Desktop does nothing

Copy the text, switch directly to Anchor, and tap **Send to Desktop**. Android only lets Anchor read the clipboard while the app is in the foreground.

Anchor skips an item when it is identical to the last item already sent.

### An image does not arrive

Only PNG images are supported. Images over the configured size limit are skipped.

### An item appears only once

Anchor prevents recently synced content from being sent back and forth between devices. This is expected.
