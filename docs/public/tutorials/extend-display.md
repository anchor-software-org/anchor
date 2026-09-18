# Use your phone as a second display

Anchor can stream a Linux display to your Android phone or tablet. With a
virtual display, you can move desktop windows onto the phone. You can also use
the phone as a touch or pointer surface.

## Requirements

- A Linux desktop running Wayland.
- Screen capture support from your compositor (`zwlr_screencopy_manager_v1`).
- A connected Android phone or tablet. See [Installation](/docs/installation).
- Sway for Anchor-managed virtual displays. Hyprland can stream its existing
  outputs, but Anchor does not currently size a new Hyprland virtual output.

Screen sharing can also work on other wlroots compositors. Virtual displays
are not available there. See [Compatibility](/docs/compatibility).

## Create and stream a virtual display

1. Open Anchor on the Linux desktop and open **Sideboat**.
2. Under **Extended display**, click **Create virtual display**.
3. Choose **Match phone** or **Custom size**.
   - Match phone uses the connected device's dimensions.
   - Custom sizes must be at least `64 × 64` and both values must be even.
4. Choose the orientation and scale when matching the phone, then click
   **Create display**.

Anchor creates a headless Sway output and places it to the right of your
existing outputs. Windows can now be moved onto it.

5. In **Output**, select the new output. It is marked **Created by Anchor**.
6. Click **Start streaming**.
7. Open Anchor on the phone. The video appears when the first frames arrive.
   Double-tap the video to enter fullscreen.

## Control the display from the phone

Open the phone's **Settings** and enable **Touch input on stream**. Then open
the stream in fullscreen.

- Tap to click.
- Drag to move the pointer or a window.
- Use the touchpad option in Settings for relative pointer movement.
- Use the close button in the top-left corner to leave fullscreen.

Keyboard input is available when the desktop compositor provides the required
virtual-keyboard protocol.

## Stream an existing display

To mirror a physical monitor, select it in the **Output** list and click
**Start streaming**. You do not need to create a virtual display.

## Stop and remove the display

1. Click **Stop streaming** on the desktop.
2. In the **Extended display** section, click **Destroy** beside the display
   created by Anchor.

Destroying the display removes the compositor output. It does not delete files
or change your normal monitor configuration.

## Troubleshooting

**No outputs are listed**

Your compositor may not provide `zwlr_screencopy_manager_v1`, or Anchor may not
be running in a Wayland session. Check [Compatibility](/docs/compatibility).

**The virtual display cannot be created**

Anchor-managed virtual displays currently require Sway. Check that `swaymsg`
is available and that the requested dimensions are even and at least `64 × 64`.

**The phone shows “Waiting for stream…”**

Confirm that both devices are connected, an output is selected, and **Start
streaming** was clicked on the desktop.

**Touch input does nothing**

Enable **Touch input on stream** on the phone and use fullscreen mode. Pointer
and keyboard support also depends on the compositor's Wayland protocols.

**The stream is slow**

Try a smaller virtual display or a lower scale. VAAPI hardware encoding lowers
CPU use when available; Anchor uses software encoding as a fallback.
