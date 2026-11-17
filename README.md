# Anchor

Anchor is software for connecting Android, IOS and Linux devices together. I'm making this software as I find current alternatives for software connectivity to be lacking, clunky, or fragmented. I would rather not use 20 apps to get an equivalent ecosystem, when I can just have one. 

Anchor currently supports:
- extending or mirroring the phone screen
- sharing clipboard text
- showing Android notifications on Linux
- reading and sending SMS and MMS from Linux
- transferring files
- using the phone camera as a Linux video device
- controlling media playback between devices

The supported desktop session is Linux Wayland. Android support requires
Android 11 (API 30) or newer. See the [compatibility guide](docs/public/getting-started/compatibility.md)
before installing.

## Install and develop

For packages and compositor requirements
[installation guide](docs/public/getting-started/installation.md).

Development commands run inside one of two [devenv](https://devenv.sh/)
environments. Enter the environment for the part of Anchor you are working on.

To build and install the Android debug app:
For development 
[docs/developer/development-environment.md](docs/developer/development-environment.md)

Quick start
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

## Feedback

For feedback or questions, email [devs@anchor-software.org](mailto:devs@anchor-software.org).

## License

Anchor is licensed under the [GPL-3.0](LICENSE).

## Disclaimer 

This project was made with assistance from agentic dev tools. See my policy on its usage [here](https://github.com/anchor-software-org/anchor/blob/main/CONTRIBUTING.md#ai-assisted-development)
