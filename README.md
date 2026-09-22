# Anchor

Anchor connects an Android phone or tablet to a Linux desktop over a local
network. It lets you use your devices together instead of moving between them.

Anchor currently supports:

- extending or mirroring the phone screen;
- controlling the phone with desktop keyboard and pointer input;
- sharing clipboard text;
- showing Android notifications on Linux;
- reading and sending SMS and MMS from Linux;
- transferring files;
- using the phone camera as a Linux video device; and
- controlling media playback between devices.

The supported desktop session is Linux Wayland. Android support requires
Android 11 (API 30) or newer. See the [compatibility guide](docs/public/getting-started/compatibility.md)
before installing.

## Install and build

See the [installation guide](docs/public/getting-started/installation.md) for
packages and compositor requirements. Development builds use
[devenv](https://devenv.sh/); see
[docs/developer/development-environment.md](docs/developer/development-environment.md)
for the full setup.

Clone with submodules — the Android SDK's QUIC transport (MsQuic) lives in a
nested submodule:

```bash
git clone --recursive <repository-url>
# or, in an existing checkout:
git submodule update --init --recursive
```

To build the Android app from a checkout:

```bash
cd anchor
./gradlew installDebug
```

The Gradle build packages whatever is in the SDK's `jniLibs/` directory and
does **not** build MsQuic itself. If the native libraries have never been
built, the APK installs fine but crashes on first connect. Build them once
first (from the repository root devenv shell):

```bash
android-msquic-build-all
```

To build the Linux desktop application:

```bash
cd anchor-desktop
devenv shell
dev
```

`dev` runs `cargo tauri dev`, which starts the frontend (pnpm + Vite) and the
Rust app together. A plain `cargo run` skips the frontend build — Tauri
embeds `frontend/dist` at compile time, so without it the build fails or the
window is blank. The desktop build needs the native libraries listed in the
installation guide. The desktop uses Quinn (pure-Rust QUIC); MsQuic is
Android-only.

## Learn more

- [Compatibility](docs/public/getting-started/compatibility.md)
- [Clipboard sync](docs/public/tutorials/clipboard-sync.md)
- [Extend the display](docs/public/tutorials/extend-display.md)
- [Use a phone as a webcam](docs/public/tutorials/phone-as-webcam.md)
- [Feature comparison](docs/public/reference/feature-comparison.md)
- [Contributing](CONTRIBUTING.md)
- [SDK and protocol](anchor-sdk/README.md)

iOS and iPadOS support is preview-only. It is not part of the supported release target yet.

## Feedback

For feedback or questions, email [devs@anchor-software.org](mailto:devs@anchor-software.org).

## License

Anchor is licensed under the [GPL-3.0](LICENSE).
