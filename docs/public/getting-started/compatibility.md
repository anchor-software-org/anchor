# Compatibility

Anchor's supported release targets are an Android phone or tablet and a Linux
desktop running Wayland.

## Linux desktop

| Requirement | Support |
|---|---|
| Operating system | Linux |
| Session | Wayland |
| Screen sharing | A compositor that provides `zwlr_screencopy_manager_v1` |
| Virtual display | Sway or Hyprland |
| Video encoding | VAAPI when available; software encoding otherwise |

Anchor can start on any Wayland compositor. Screen sharing requires the
`zwlr_screencopy_manager_v1` protocol, which is commonly available on wlroots
compositors such as Sway, Hyprland, river, Wayfire, and labwc. Without it,
screen sharing is unavailable.

Virtual displays are available only on Sway and Hyprland. Anchor creates them
through the compositor's command-line tools (`swaymsg` or `hyprctl`).

Clipboard support depends on the compositor:

- wlroots data-control on compositors that provide it;
- `ext-data-control` on compositors that provide the standardized protocol;
- XWayland on GNOME/Mutter, when XWayland is enabled and `DISPLAY` is set.

Other desktop features use Wayland input and output protocols. Their
availability depends on the protocols exposed by your compositor.

VAAPI reduces CPU use but is optional. Anchor falls back to software H.264
encoding when hardware encoding is unavailable.

## Android

The Android app requires Android 11 or newer (API level 30).

It connects to a Linux desktop over Wi-Fi on the same local network. USB
connections use an ADB reverse tunnel and require ADB access to the device.

Android restricts background clipboard access. To send the current phone
clipboard to the desktop, open Anchor and use **Send to Desktop**.

## iOS and iPadOS

iOS support is preview-only and is not part of the supported release target.
The app project targets iOS 17, but it still uses the legacy network plugin and
has not completed the Protocol v1 and Network.framework transport migration.
The Swift SDK is also preview-only.

## Not supported

- Windows or macOS desktop hosts
- Linux X11 desktop sessions
- Screen sharing on Wayland compositors without `zwlr_screencopy_manager_v1`
- Virtual displays outside Sway and Hyprland
