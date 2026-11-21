# Vesper desktop

Declaring `vesper` is all it takes: forge installs the [Vesper](https://github.com/noxilith/VesperDE) desktop **and sets up everything
it needs**, and taking `vesper` out of the declaration takes all of it away again. It is early software, not yet tested on much real hardware.

```bash
sudo forge install vesper     # then reboot
sudo forge remove vesper      # undoes everything below
```

## What installing it does

* Installs the desktop and what it uses: the compositor, bar, launcher and file manager, Xwayland, the screen-sharing portals,
  PipeWire and WirePlumber, NetworkManager, BlueZ, polkit, UPower, power-profiles-daemon, udisks2, greetd, foot, fonts,
  tesseract (copy text from the screen), wf-recorder and ffmpeg (recording).
* Turns on the services it needs, under OpenRC or systemd alike: `seatd`, `greetd` (the login screen), `NetworkManager`, `bluetooth`,
  `upower`, `power-profiles-daemon`, `udisks2`.
* Hands networking to NetworkManager. forge's own network setup (the `network` service, or systemd-networkd and its Wi-Fi
  supplicant) is switched off while the desktop is installed, so only one thing configures your interfaces. Connect to Wi-Fi from the bar.
* Creates the `greeter` account the login screen runs as, and adds every user in `system.toml` to `video`, `input` and `audio`.
* Starts the audio stack with your session.

Reboot and you get the Vesper login screen on virtual terminal 7; log in as any user (F3 picks the session). The text consoles
on Ctrl+Alt+F1 to F6 still work if anything goes wrong.

## What removing it does

forge removes the packages that only the desktop needed, turns the services off, deletes the `greeter` account, takes back the
groups it added (a group you wrote into `system.toml` yourself is kept), and gives networking back to forge's own setup. Two empty
directories, `/var/lib/greeter` and `/var/cache/vesper-greeter`, may be left behind. Your own files are never touched.

## Without the login screen

`vesper-session` starts the desktop from a text console (use it, not `vesper` directly: the script prepares the environment).

## What is in and what is not

| In | Not yet |
|---|---|
| greetd, udisks2, tesseract, wf-recorder, ffmpeg, Xwayland, screen-sharing portals, PipeWire/WirePlumber, NetworkManager (WPA-PSK Wi-Fi, Ethernet), BlueZ, brightnessctl, power-profiles-daemon, polkit, foot, fonts, FUSE | Vulkan, 802.1X enterprise Wi-Fi (NetworkManager is built without a crypto library), the GTK portal backend (needs GTK), udisks2 encryption/RAID/NVMe plugins (the daemon logs that they did not load) |

## Also installed with it now

`matugen` (colour themes from your wallpaper), `rtkit` (real-time priority for audio), `linux-firmware-bluetooth` (Realtek and
Qualcomm Bluetooth firmware), and Bluetooth audio (PipeWire is built with SBC support). `gettext` (translation tools) is available
as its own package. Still not included: the GTK portal backend, Vulkan, and Bluetooth codecs other than SBC.
