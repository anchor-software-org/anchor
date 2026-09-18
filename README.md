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

## Install and develop

See the [installation guide](docs/public/getting-started/installation.md) for
packages and compositor requirements.

Development commands run inside one of two [devenv](https://devenv.sh/)
environments. Enter the environment for the part of Anchor you are working on.

To build and install the Android debug app:

```bash
devenv shell
android-install
```

To run the Linux desktop app in development:

```bash
cd anchor-desktop
devenv shell
dev
```

See the [development environment guide](docs/developer/development-environment.md)
for SDK setup, checks, production builds, and the complete command reference.

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
