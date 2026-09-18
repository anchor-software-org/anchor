# Contributing to Anchor

Anchor includes a Linux desktop app, an Android app, SDKs, a shared protocol,
and the project website. Contributions to all of these parts are welcome.

## Before you start

Read the [development environment guide](docs/developer/development-environment.md).
Clone the repository with its submodules:

```bash
git clone --recursive <repository-url>
```

If you already cloned the repository, run:

```bash
git submodule update --init --recursive
```

## Make a change

Create a branch from `main`. Keep changes focused and explain the reason for
the change in your pull request.

Use the existing code style. Format Rust changes before committing:

```bash
cd anchor-desktop
cargo fmt --all
```

Do not commit credentials, private keys, personal data, build output, or local
IDE files. Do not edit generated protocol files unless the source schema or
generation step also changed.

## Run checks

Run the checks for the area you changed.

Linux desktop:

```bash
cd anchor-desktop
cargo fmt --all --check
cargo clippy --all-targets
cargo test -- --nocapture
```

The desktop frontend has its own checks:

```bash
cd anchor-desktop/frontend
pnpm install --frozen-lockfile
pnpm run check
pnpm run build
```

Rust SDK:

```bash
cd anchor-sdk/rust
cargo test
```

Android app and Kotlin SDK:

```bash
cd anchor
./gradlew :anchorSdk:testDebugUnitTest :app:testDebugUnitTest
```

For Android builds, emulator tests, MsQuic, and native dependencies, use the
[development environment guide](docs/developer/development-environment.md).
For SDK and protocol changes, also read the [transport contract](docs/developer/protocol/transport-contract.md)
and [`anchor-sdk/protocol/VERSIONING.md`](anchor-sdk/protocol/VERSIONING.md), which explains which
of the three version numbers (protocol major/minor, a capability's major, or an SDK's own package
version) a given change should bump.

## Pull requests

Open a pull request against `main` after the relevant checks pass. Include:

- what changed and why;
- how you tested it;
- user-visible behavior or compatibility impact;
- protocol or SDK version impact, if any.

Keep generated changes separate and easy to review. Update relevant tests when
behavior changes. CI runs the repository checks on pull requests; a pull
request must pass CI before it can be merged.

## Repository layout

| Path | Contents |
| --- | --- |
| `anchor-desktop/` | Linux desktop app and Tauri frontend |
| `anchor/` | Android app and Android build files |
| `anchor-sdk/` | Rust, Kotlin, Swift-preview, and protocol SDK code |
| `docs/public/` | User-facing documentation; website deployment lives in the separate `anchor-website` repository |
| `docs/developer/` | Development, architecture, and protocol notes |
| `docs/maintainers/` | Maintainer performance baselines |
| `anchor-wl-clipboard` | External Wayland clipboard library dependency |

## Questions and reports

Use GitHub issues for public bugs and feature requests. Email
`devs@anchor-software.org` for private feedback, support, or security reports.
Do not post credentials, private keys, certificate pins, or other sensitive
information in an issue or pull request.
