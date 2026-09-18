# Use your phone as a webcam

Anchor can expose your Android phone as a webcam on a Linux desktop. The
desktop creates an `Anchor Camera` device at `/dev/video42`; apps such as OBS,
Zoom, and Discord can use it as a normal camera.

## Before you start

- Install Anchor on your Linux desktop and Android phone. See
  [Installation](/docs/installation).
- Pair the phone with the desktop.
- Install `v4l2loopback` for your Linux distribution. Anchor loads the module,
  but it must be installed first.
- Connect the phone to the desktop in Anchor.

## Set up the desktop

1. Open Anchor and select **Settings**.
2. Find **Phone As Camera**.
3. Under **Setup**, click **Create camera**. Enter your password if the system
   asks for administrator permission.
4. Wait until the status is **ready** and the device is `/dev/video42`.
5. If more than one phone is connected, select the phone under **Camera
   source**. **Automatic** lets Anchor choose a connected camera-capable phone.

To recreate the camera when Anchor starts, enable **Enable on startup**.

## Start the camera on Android

1. Open Anchor and open **Camera** from the navigation menu.
2. Grant camera permission when Android asks.
3. Confirm that the status says **Ready to stream**.
4. Tap the camera button. The status changes to **fps · streaming**.
5. Select **Anchor Camera** in your Linux app.

Tap the camera button again to stop. Use the flip button to switch between the
front and rear cameras.

## Change the stream settings

On Android, open **Stream settings** with the gear button. You can choose 15,
24, 30, 48, or 60 FPS and adjust the bitrate from 500 to 8000 kbps.

On the desktop, use **Stream defaults** under **Phone As Camera**. If Android
settings are overriding the desktop, tap **Use desktop defaults** on the phone.

The desktop also provides live controls for zoom, exposure, torch, and camera
switching. The torch works only with the rear camera when the phone has a flash.

## Troubleshooting

### Create camera fails

Install the `v4l2loopback` package and try **Create camera** again. On Ubuntu or
Debian, the package is usually named `v4l2loopback-dkms`. If the module does
not load, it may not be built for your running kernel. Secure Boot can also
prevent an unsigned module from loading.

### The camera app cannot find Anchor Camera

Check that **Phone As Camera** shows **ready** and `/dev/video42`. Restart the
camera app after creating the device; some apps cache their camera list.

### The preview is black or frozen

Open **Camera** on the phone and check the status. Start streaming only when it
says **Ready to stream**. If it says **Not connected**, reconnect the phone in
Anchor and try again.

### The video is slow or poor quality

Lower the FPS or bitrate in **Stream settings** on Android. A weak connection
between the phone and desktop can also cause dropped frames.
