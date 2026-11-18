# Vesper desktop

`sudo forge install vesper` installs the [Vesper](https://github.com/noxilith/VesperDE) desktop and everything it uses. It is
early software: the compositor, bar, launcher and file manager run, but this packaging has not been through much real hardware yet.
It works the same under OpenRC and systemd.

## Start it

Declaring `vesper` also turns on the background services it needs under whichever init you use (`seatd`, `bluetooth`, `upower`,
`power-profiles-daemon`, `udisks2`); forge does it on the same apply. Your user needs the `video` and `input` groups, which you
give in `/etc/forge/system.toml`, for example:

```toml
[[users]]
name = "you"
groups = ["wheel", "video", "input", "audio"]
```

Then apply, log in on a text console and run:

```bash
sudo forge apply
vesper-session
```

Use `vesper-session`, not `vesper`: the script prepares the environment (a runtime directory, the seat daemon, the audio stack
with PipeWire, WirePlumber and the PulseAudio bridge, a D-Bus session).

## Wi-Fi (NetworkManager) is opt-in

NetworkManager would fight forge's own network setup, so it is not turned on for you. If you want the bar's Wi-Fi switch and `nmcli`,
add it yourself: `services = ["NetworkManager"]` in `system.toml` (and remove forge's wired/wireless setup if you changed it).

## What is in and what is not

| In | Not yet |
|---|---|
| greetd, udisks2, tesseract, wf-recorder, ffmpeg, Xwayland, screen-sharing portals, PipeWire/WirePlumber, NetworkManager (WPA-PSK Wi-Fi, Ethernet), BlueZ, brightnessctl, power-profiles-daemon, polkit, foot, fonts, FUSE | Intel graphics (Mesa's iris driver needs clang/SPIR-V), Vulkan, Bluetooth audio (needs `sbc`), 802.1X enterprise Wi-Fi (NetworkManager is built without a crypto library), the GTK portal backend (needs GTK), matugen, udisks2 encryption/RAID/NVMe plugins (the daemon logs that they did not load) |

## Login screen (greetd)

`greetd` is installed but not enabled. For Vesper's own login screen create the unprivileged user, use its config and enable it by
adding `greetd` to `services` in `system.toml`:

```bash
sudo useradd -r -s /usr/bin/nologin greeter
sudo cp /usr/share/vesper/greetd-vesper.toml /etc/greetd/config.toml
```

Skip it if you prefer to log in on the console and run `vesper-session`.
