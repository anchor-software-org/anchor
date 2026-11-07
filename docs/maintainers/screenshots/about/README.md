# About settings screenshots

- `about-android.png`: native debug APK in an isolated Android emulator, API 37.1, x86_64, 1080 × 2400. Shows version 1.0.2, build 102, and source commit `bacffb9`.
- `about-desktop.png`: About panel at the default 1180 × 780 desktop window size.
- `about-compact.png`: Settings at the minimum 880 × 620 desktop window size.

Desktop captures use Chromium with mocked Tauri IPC: Anchor 1.0.0, source commit `bacffb9`, Sway, and no paired devices. Browser checks covered all four project-link command arguments, the GitHub issue URL, missing metadata, capabilities, the Settings heading, and horizontal overflow at the minimum size.

Android checks confirmed the version/build fields and browser launches for the source commit, repository, website, and GitHub issue links. Desktop system-browser launch still needs native verification.
