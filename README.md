# Anchor

Anchor is software for connecting Android, IOS and Linux devices together. I'm making this software as I find current alternatives for software connectivity to be lacking, clunk, or fragmented. I would rather not use 20 apps to get an equivalent ecosystem, when I can just have one.

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

## Install and build

For packages and compositor requirements
[installation guide](docs/public/getting-started/installation.md).

For development 
[docs/developer/development-environment.md](docs/developer/development-environment.md)

Quick start
```bash
git clone --recursive <repository-url>
cd anchor-desktop && devenv shell
dev
```

## Roadmap 

Right now this is very much in an early stage, I'm sure there are bugs for different hardware or devices that I have yet to squash. 
Some current problems are support for different compositors, since I use sway it is the most supported along with Hyprland but 
I'm looking for help in testing and extending compatibility to more systems. 

Some future features that I would appreciate help in making or that I plan on making
- Controlling my phone through my desktop without a janky solution (similar to windows phone for android)
- Taking calls
- Support for bluetooth 
- Wifi direct connections (was attempted before but usually the hardware is the constraint) 
- Audio passthrough, play phone audio through desktop and vice versa 

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

This project was made with assistance from agentic dev tools
