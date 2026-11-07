# About settings screenshots

Captured in Chromium from the Vite frontend with mocked Tauri IPC. The fixture uses Anchor 1.0.0, source commit `015e5c8`, Sway, and no paired devices.

- `about-desktop.png`: About panel at the default 1180 × 780 window size.
- `about-compact.png`: Settings at the minimum 880 × 620 window size.

Browser checks covered the source-link command arguments, copying all three support-detail lines, selecting the text when clipboard access is denied, missing metadata, the capabilities menu, and horizontal overflow at the minimum size. These captures do not verify the native WebKit clipboard or system browser launch.
