# Vesper desktop

`sudo forge install vesper` installs the [Vesper](https://github.com/noxilith/VesperDE) desktop and everything it uses. It is
early software: the compositor, bar, launcher and file manager run, but this packaging has not been through much real hardware yet.

## Start it

Turn on the services it needs, once, then start the session from a text console (not from inside another desktop).

**OpenRC (the default):**

```bash
sudo rc-update add dbus default
sudo rc-update add seatd default
sudo rc-update add NetworkManager default
sudo rc-update add bluetooth default
sudo rc-update add upower default
sudo rc-update add power-profiles-daemon default
sudo usermod -aG video,input,seat "$USER"    # seat only if the group exists
sudo reboot
vesper-session
```

PipeWire, WirePlumber and the PulseAudio bridge are started by `vesper-session` itself on OpenRC. polkit's daemon (`polkitd`) is
started by D-Bus when something asks for it.

**systemd:** logind is the seat provider, so no `seatd` is needed.

```bash
sudo systemctl enable --now NetworkManager bluetooth upower power-profiles-daemon
systemctl --user enable --now pipewire wireplumber pipewire-pulse
vesper-session
```

## What is in and what is not

| In | Not yet |
|---|---|
| greetd, udisks2, tesseract, wf-recorder, ffmpeg, Xwayland, screen-sharing portals, PipeWire/WirePlumber, NetworkManager (WPA-PSK Wi-Fi, Ethernet), BlueZ, brightnessctl, power-profiles-daemon, polkit, foot, fonts, FUSE | Intel graphics (Mesa's iris driver needs clang/SPIR-V), Vulkan, Bluetooth audio (needs `sbc`), 802.1X enterprise Wi-Fi (NetworkManager is built without a crypto library), the GTK portal backend (needs GTK), matugen, udisks2 encryption/RAID/NVMe plugins (the daemon logs that they did not load) |

## Login screen (greetd)

`greetd` is installed but not enabled. For Vesper's own login screen create the unprivileged user, use its config and enable it:

```bash
sudo useradd -r -s /usr/bin/nologin greeter
sudo cp /usr/share/vesper/greetd-vesper.toml /etc/greetd/config.toml
sudo rc-update add greetd default        # systemd: sudo systemctl enable greetd
```

Skip it if you prefer to log in on the console and run `vesper-session`.
