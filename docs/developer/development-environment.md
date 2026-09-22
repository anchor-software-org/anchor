# Development environment

Anchor has two devenv environments. Run each command from its own directory:

- repository root: Android app and Kotlin SDK;
- `anchor-desktop/`: Linux desktop app.

Both environments are pinned by the `devenv.lock` beside their `devenv.nix`.

## First setup

Install Nix and [devenv](https://devenv.sh/). Clone with submodules because
the Kotlin SDK includes MsQuic:

```sh
git clone --recursive <repository-url>
cd anchor
```

For an existing checkout:

```sh
git submodule update --init --recursive
```

### Submodules and native libraries

Anchor vendors its QUIC implementation differently per platform:

- **Android** uses [MsQuic](https://github.com/microsoft/msquic) at
  `anchor-sdk/kotlin/src/main/cpp/third_party/msquic`. MsQuic itself carries a
  nested submodule, `submodules/quictls` (the OpenSSL fork it uses for TLS)
- **Desktop** uses [Quinn](https://github.com/quinn-rs/quinn), a pure-Rust QUIC
  implementation fetched by Cargo. There is nothing to build or initialize.

The MsQuic build outputs are **not checked in**. The Gradle build packages whatever is present in `anchor-sdk/kotlin/src/main/jniLibs/<abi>/` — a fresh clone ships no `.so` files there, yet the APK still builds and installs successfully. 

Build the native libraries once after cloning, and again whenever the MsQuic submodule pin moves or `jniLibs/` is cleaned:

```sh
android-msquic-build-all
```
This produces `libmsquic.so` and `libanchor_msquic_jni.so` for `arm64-v8a`, `armeabi-v7a`, and `x86_64`. To build a single ABI, use `android-msquic-build <abi>`.

## Android

From the repository root, enter the Android shell:

```sh
devenv shell
```

The Android SDK and NDK are not installed by Nix. Install them with Android's
SDK tools. Set `ANDROID_SDK_ROOT` if the SDK is not at `~/Android/Sdk`.
`ANDROID_NDK_ROOT` is optional; otherwise Anchor selects the newest NDK under
`$ANDROID_SDK_ROOT/ndk`.

Verify the paths before building:

```sh
android-env
```

Use these commands inside that shell:

| Command | What it does |
| --- | --- |
| `android-build` | Builds `:app:assembleDebug`. |
| `android-sdk-test` | Runs Kotlin SDK debug unit tests. |
| `android-msquic-build arm64-v8a` | Builds MsQuic for one ABI. |
| `android-msquic-build-all` | Builds MsQuic for `arm64-v8a`, `armeabi-v7a`, and `x86_64`. |
| `android-install` | Builds if needed, then installs the debug APK with ADB. |

`android-install` uses the first connected device. Set `ANDROID_SERIAL` when
more than one device is connected.

## Linux desktop

Enter the desktop shell from `anchor-desktop/`:

```sh
cd anchor-desktop
devenv shell
```

Use these commands inside that shell:

| Command | What it does |
| --- | --- |
| `dev` | Runs `cargo tauri dev`. |
| `check-all` | Runs `cargo check` and the frontend type check. |
| `build-image` | Builds the portable AppImage through the Ubuntu 22.04 container builder. |

Run Rust tests and formatting directly when needed:

```sh
cargo fmt --check
cargo test
```

`build-image` uses Docker by default. Set `CONTAINER_ENGINE` to a compatible
engine if needed. It writes the AppImage under
`anchor-desktop/target/appimage/`.

### How the desktop build is wired

The desktop is a Tauri app: a Rust binary hosting a WebKitGTK webview whose UI
lives in `frontend/` (Svelte + Vite, built with **pnpm** — not npm). The
coupling is in `anchor-desktop/tauri.conf.json`:

- `cargo tauri dev` runs `beforeDevCommand` (`pnpm run dev`) to start the Vite
  dev server on port 1420 and points the webview at it.
- `cargo tauri build` runs `beforeBuildCommand` (`pnpm run build`) and embeds
  the resulting `frontend/dist` into the binary.

Entering the devenv shell runs `pnpm install` automatically
(`languages.javascript.pnpm.install.enable` in `devenv.nix`). Outside devenv,
run `pnpm --dir frontend install` yourself first.

`frontend/dist` is not checked in, and `cargo run`/`cargo build` do **not**
invoke the Tauri hooks — Tauri embeds `frontend/dist` at compile time, so a
bare `cargo run --release` on a fresh checkout fails or produces a blank
window. Either use `dev`/`cargo tauri build`, or build the frontend first:

```sh
pnpm --dir frontend install
pnpm --dir frontend build
```

The Sideboat baselines also need a host `ffmpeg`; `--headed` needs `ffplay`.
VA-API benchmarks additionally need a usable DRM render device and driver.

## Update pinned inputs

After changing either `devenv.nix`, update the lock in the same directory:

```sh
devenv update
```

Review the resulting `devenv.lock`. Do not edit `.devenv` files.
