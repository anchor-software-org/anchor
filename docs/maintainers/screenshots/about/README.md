# About settings screenshots

- `about-android.png` and `about-android-light.png`: native debug APK in an isolated Android emulator, API 37.1, x86_64. The viewport is 1080 × 2340 at 480 dpi with font scale 1.15, matching the phone used for preview review. Both captures show version 1.0.2, build 102, and source commit `c7aaa1c`.
- `about-desktop.png`: About panel at the default 1180 × 780 desktop window size.
- `about-compact.png`: Settings at the minimum 880 × 620 desktop window size.

Desktop captures use Chromium with mocked Tauri IPC: Anchor 1.0.0, source commit `c7aaa1c`, Sway, and no paired devices. Browser checks covered the logo, all four project-link command arguments, absence of copy/email controls, capabilities, and horizontal overflow at the minimum size.

Android About was reviewed on a Galaxy S25 running Android 17 in dark mode using a separate preview package. The screenshots above are emulator captures. Emulator checks covered the version/build fields in both themes at font scale 1.15; earlier checks verified browser launches for the source commit, repository, website, and GitHub issue links. Desktop system-browser launch still needs native verification.
