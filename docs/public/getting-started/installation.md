# Installation

Anchor has an Android app and a Linux desktop app. Install the Linux desktop
app on the computer you want to connect to your phone.

## Linux

### AppImage

Download the AppImage for your CPU from the [Downloads](/downloads) page.
Anchor provides builds for x86-64 and ARM64.

Make the file executable and start it:

```bash
chmod +x Anchor-x86_64.AppImage
./Anchor-x86_64.AppImage
```

Replace `x86_64` with `aarch64` when using the ARM64 build.

Most desktop Linux systems can run the AppImage as-is. If it reports a missing
`libfuse.so.2`, install FUSE 2:

```bash
# Ubuntu or Debian
sudo apt install libfuse2

# Arch Linux
sudo pacman -S fuse2
```

If FUSE is unavailable, run the AppImage without mounting it:

```bash
./Anchor-x86_64.AppImage --appimage-extract-and-run
```

Anchor uses the host's GTK, WebKitGTK, Wayland, and graphics libraries. On a
minimal installation, install the desktop libraries provided by your
distribution. A normal desktop installation already includes them.

### Build from source

Source builds use [devenv](https://devenv.sh/) and Nix. Install both, then run:

```bash
cd anchor-desktop
devenv shell
dev
```

The `dev` command starts the desktop app and its frontend development server.

## Phone as a webcam

The webcam feature needs the `v4l2loopback` kernel module. Install the package
for your distribution, then load it:

```bash
sudo modprobe v4l2loopback
```

Examples:

```bash
# Ubuntu or Debian
sudo apt install v4l2loopback-dkms

# Fedora (RPM Fusion)
sudo dnf install akmod-v4l2loopback kernel-devel-$(uname -r)

# Arch Linux (AUR)
yay -S v4l2loopback-dkms
```

Anchor can load the module from **Settings → Camera**. The feature creates
`/dev/video42`. If the module does not load, make sure it was built for the
running kernel. Secure Boot may also require you to sign the module.

## Next step

After installing both apps, follow [Compatibility](/docs/compatibility) before
pairing your devices.
