# Feature reference

Anchor connects an Android phone to a Linux desktop. The Android and Linux
clients are the supported release platforms. iOS support is preview-only.

## What Anchor can do

| Feature | What it supports |
| --- | --- |
| Screen streaming | Show a Linux desktop output on the phone. Mirror a physical output or use a virtual output as a second display. |
| Touch input | Use the phone screen as pointer and keyboard input for the streamed desktop output. |
| Clipboard sync | Sync text and images from the desktop to the phone. Send text from the phone to the desktop when requested. |
| File transfer | Send files in either direction. Files received on Android are saved to Downloads. |
| SMS | Read phone conversations and send SMS from the Linux desktop. |
| Notifications | Show phone notifications on the desktop and desktop notifications on the phone. |
| Media controls | View playback state and control supported media sessions on either device. |
| Remote input | Use the phone as a desktop touchpad and keyboard. |
| Phone camera | Use the Android camera as a Linux webcam through a V4L2 loopback device. |
| Desktop commands | Create trusted commands on Linux and run them from the phone. |

## Platform support

### Linux desktop

- Wayland is required.
- Screen capture requires the compositor's `wlr-screencopy` support.
- Clipboard support depends on the data-control protocol exposed by the
  compositor. GNOME needs XWayland for background clipboard monitoring.
- The phone-camera feature requires `v4l2loopback`.
- Hardware H.264 encoding uses VAAPI when available; software encoding is the
  fallback.

### Android phone

- Android 11 (API 30) or newer is supported.
- Connect over Wi-Fi on the same local network, or use USB with an ADB reverse
  tunnel.
- Android permissions are required for features such as SMS, notifications,
  camera, and media controls.

### iOS

iOS is not part of the supported Android + Linux release pair. Treat the iOS
client as preview software and expect feature and transport differences.

## Not currently provided

Anchor does not provide desktop phone calls, contact synchronization, phone
filesystem browsing from a desktop file manager, or instant hotspot setup.

Feature availability can also vary with the Linux compositor, Android version,
device permissions, and connected hardware.
