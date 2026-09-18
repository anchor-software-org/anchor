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
packages and compositor requirements.

To build the Android app from a checkout:

```bash
cd anchor
./gradlew installDebug
```

To build the Linux desktop application:

```bash
cd anchor-desktop
cargo run --release
```

The desktop build needs the native libraries listed in the installation guide.

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
