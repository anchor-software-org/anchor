# Compatibility

Anchor's supported release targets are an Android phone or tablet and a Linux
desktop running Wayland.

## Linux desktop

| Requirement | Support |
|---|---|
| Operating system | Linux |
| Session | Wayland |
| Screen sharing | A compositor that provides `zwlr_screencopy_manager_v1` |
| Virtual display | Sway |
| Video encoding | VAAPI when available; software encoding otherwise |

Anchor can start on any Wayland compositor. Screen sharing requires the
`zwlr_screencopy_manager_v1` protocol, which is commonly available on wlroots
compositors such as Sway, Hyprland, river, Wayfire, and labwc. Without it,
screen sharing is unavailable.

Anchor creates virtual displays through `swaymsg`, so they are available only
on Sway. On Hyprland you can stream an existing output, but Anchor does not
create or size a new virtual output there.

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

It connects to a Linux desktop over Wi-Fi on the same local network or 
can be relayed through the likes of Tailscale or other similar services. 

A wired connection over the USB cable is also supported: plug the phone into
the desktop and enable **USB tethering** (usually under Settings → Network &
internet → Hotspot & tethering; the app has a "USB tethering" shortcut under
Settings → Connection Status). The desktop receives an address on the new
link automatically and the phone connects over the cable, which is preferred
over Wi-Fi whenever both paths reach the same desktop.

When USB tethering starts, some desktops add a default route through the
phone and may send their own internet traffic over the phone's mobile data.
If that is unwanted, configure the new wired connection to not be a default
route — for NetworkManager, `nmcli connection modify <connection>
ipv4.never-default yes` (or enable "Use this connection only for resources on
its network" in the connection editor).

To send the current phone clipboard to the desktop, open Anchor and use **Send to Desktop**.
The desktop -> android clipboard sync happens automatically.

## iOS and iPadOS

iOS support is preview-only and is not part of the supported release target.

For now there is a working version for Ipad, but it is still very much experimental 
and needing development.

The iOS app can reach the desktop over a cable too. Plug the device in and
either enable **Personal Hotspot** on it (Settings → Personal Hotspot → Allow
Others to Join), or share the desktop's connection to the device over USB
(macOS: System Settings → General → Sharing → Internet Sharing). Both give
the link an address automatically; the app probes local interfaces and lists
answering desktops under Nearby, preferring the wired path for new
connections.

As with Android tethering, enabling Personal Hotspot can make the desktop
route its own internet traffic through the phone's mobile data — configure
the new wired connection to not be a default route if that is unwanted
(NetworkManager: `nmcli connection modify <connection> ipv4.never-default yes`).

## Not supported

- Windows or macOS desktop hosts
- Linux X11 desktop sessions
- Screen sharing on Wayland compositors without `zwlr_screencopy_manager_v1`
