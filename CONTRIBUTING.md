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

Use the existing code style. Format Rust changes from the desktop devenv shell:

```bash
cd anchor-desktop
devenv shell
cargo fmt --all
```

Do not commit credentials, private keys, personal data, build output, or local
IDE files. Do not edit generated protocol files unless the source schema or
generation step also changed.

## Run checks

Run CI jobs with the `ci` command inside either devenv shell. It selects the
environment for each job, so you can run the default local CI jobs from the
repository root:

```bash
devenv shell
ci all
```

Run an individual job when you only need checks for one area:

```bash
ci check    # desktop Rust and frontend checks
ci sdk      # Rust and Kotlin SDK source conformance
ci android  # Android tests and debug APK
ci desktop  # Linux desktop release binary
```

The Android job needs an installed Android SDK and NDK. The AppImage job is
optional and builds in its Ubuntu container:

```bash
ci appimage
```

On macOS, run the iOS CI checks inside the repository-root devenv shell:

```bash
ios-verify
```

For environment setup and job details, see the
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

## AI Assisted Development

I'm not looking for slop PRs, for each PR you should know what it is doing. 
Main guideline is to not be lazy

Also do not specify the tag of the AI being used, thats just noise. 

You should be able to answer questions about the code, and be willing to find out the answers

Don't throw code over the wall 

Adopted from [here](https://community.kde.org/Guidelines_and_HOWTOs/Maintainers_and_Contributions)
