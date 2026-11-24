#!/bin/sh
# SingularitySYS kernel configuration, run inside a kernel source tree. One script for both users:
# the `linux` recipe (the live ISO's kernel) and `forge kernel` (which embeds this file).
#
#   sh full-config.sh full        defconfig base + every driver as a module
#   sh full-config.sh check       only verify that an existing .config has what a Singularity system needs
#   sh full-config.sh essential   re-assert what a Singularity system needs on an existing .config
#                                 (what `forge kernel --auto` runs after trimming with localmodconfig)
#
# KCFG_TMP is a scratch directory (default /build); KCFG_CC the compiler (default "gcc -std=gnu11":
# gcc 15 defaults to C23, which the 6.12-7.x EFI stub does not build as).
set -e
mode=${1:?usage: full-config.sh full|essential|check}
tmp=${KCFG_TMP:-/build}
cc=${KCFG_CC:-gcc -std=gnu11}
export KBUILD_BUILD_TIMESTAMP='1980-01-01'
export KBUILD_BUILD_USER=forge KBUILD_BUILD_HOST=singularity
kmake() { make ARCH=x86_64 CC="$cc" "$@"; }
c=./scripts/config

# What the live system needs to reach its root, with no initramfs able to load modules: built in. Also what systemd
# needs (control groups with accounting, fanotify, autofs, tmpfs ACLs, seccomp), which OpenRC does not mind.
builtin="BLK_DEV_INITRD RD_GZIP RD_ZSTD DEVTMPFS DEVTMPFS_MOUNT TMPFS BLK_DEV_LOOP
 SQUASHFS SQUASHFS_FILE_DIRECT SQUASHFS_DECOMP_MULTI SQUASHFS_ZSTD SQUASHFS_XZ
 ISO9660_FS JOLIET ZISOFS OVERLAY_FS BLK_DEV_SR SCSI_SCAN_ASYNC
 ATA SATA_AHCI ATA_PIIX BLK_DEV_NVME USB_STORAGE USB_UAS
 USB_XHCI_HCD USB_XHCI_PCI USB_EHCI_HCD USB_EHCI_PCI USB_OHCI_HCD USB_UHCI_HCD
 MMC MMC_BLOCK MMC_SDHCI MMC_SDHCI_PCI
 VIRTIO_BLK SCSI_VIRTIO VIRTIO_PCI VIRTIO_NET VIRTIO_CONSOLE
 IGB IGC TIGON3 ALX ATL1C USB_USBNET USB_NET_CDCETHER USB_NET_CDC_NCM USB_NET_AX8817X
 USB_NET_AX88179_178A USB_RTL8152
 FUSE_FS BTRFS_FS VFAT_FS FAT_FS MSDOS_FS EXT4_FS NLS_CODEPAGE_437 NLS_ISO8859_1 NLS_UTF8
 EFI EFI_STUB EFIVAR_FS SYSFB_SIMPLEFB DRM_SIMPLEDRM DRM_FBDEV_EMULATION
 CGROUPS MEMCG CGROUP_PIDS CGROUP_FREEZER CGROUP_DEVICE CGROUP_CPUACCT CGROUP_SCHED BLK_CGROUP BPF_SYSCALL CGROUP_BPF FANOTIFY AUTOFS_FS
 TMPFS_XATTR TMPFS_POSIX_ACL SECCOMP SECCOMP_FILTER PSI INOTIFY_USER SIGNALFD TIMERFD EPOLL FHANDLE
 IKCONFIG IKCONFIG_PROC NAMESPACES USER_NS UTS_NS IPC_NS PID_NS NET_NS SERIAL_8250 SERIAL_8250_CONSOLE INPUT_KEYBOARD KEYBOARD_ATKBD HID_GENERIC USB_HID"

essential_settings() {
    for o in $builtin; do $c -e $o; done
    # No debug information: it is large, and the build must not record paths.
    $c -d DEBUG_INFO -d DEBUG_INFO_DWARF4 -d DEBUG_INFO_DWARF5 -d DEBUG_INFO_BTF -e DEBUG_INFO_NONE
    # New compilers warn about things old kernels predate; warnings must not fail the build.
    $c -d WERROR
    # KCFG_LOCALVERSION makes the release name unique per kernel build (7.3.3-r7): two builds of the same kernel then
    # have different files in /boot and different module directories, so a generation can really boot the old one.
    $c --set-str LOCALVERSION "${KCFG_LOCALVERSION:-}" -d LOCALVERSION_AUTO
}

# The framebuffer console depends on options only visible after DRM_FBDEV_EMULATION is on, so it is
# switched on in a second pass (set earlier it is silently dropped, which left the first live ISO
# with a black screen on every UEFI machine: simpledrm without fbcon draws no text).
fbcon() {
    for o in FRAMEBUFFER_CONSOLE FRAMEBUFFER_CONSOLE_DETECT_PRIMARY FONT_SUPPORT FONTS FONT_8x16 FONT_TER16x32; do
        $c -e $o
    done
}

# olddefconfig drops options whose dependencies are not met without saying so. Everything the live
# system needs must have survived, or the build stops here.
check_essential() {
    missing=
    for o in BLK_DEV_INITRD DEVTMPFS BLK_DEV_LOOP SQUASHFS SQUASHFS_ZSTD ISO9660_FS OVERLAY_FS BLK_DEV_SR \
             SATA_AHCI BLK_DEV_NVME USB_STORAGE USB_XHCI_HCD USB_EHCI_HCD VIRTIO_BLK VIRTIO_PCI \
             BTRFS_FS VFAT_FS EFI EFI_STUB DRM_SIMPLEDRM DRM_FBDEV_EMULATION FB_CORE FRAMEBUFFER_CONSOLE \
             VT VT_CONSOLE SERIAL_8250_CONSOLE KEYBOARD_ATKBD HID_GENERIC USB_HID \
             NAMESPACES USER_NS UTS_NS IPC_NS PID_NS NET_NS CGROUPS MEMCG FANOTIFY AUTOFS_FS SECCOMP_FILTER \
             E1000E R8169 IGB IGC TIGON3 USB_RTL8152 USB_NET_CDCETHER; do
        grep -q "^CONFIG_$o=y" .config || missing="$missing $o"
    done
    if [ -n "$missing" ]; then echo "kernel options that did not survive olddefconfig:$missing" >&2; exit 1; fi
}

case "$mode" in
check)
    check_essential
    exit 0
    ;;
essential)
    essential_settings
    kmake olddefconfig >/dev/null
    fbcon
    kmake olddefconfig >/dev/null
    check_essential
    exit 0
    ;;
full) ;;
*) echo "unknown mode $mode" >&2; exit 2 ;;
esac

# ---- full ---------------------------------------------------------------------------------------
# Compatibility with hardware nobody has tried yet: every driver the kernel has, as a MODULE. The list of
# candidates comes from allmodconfig (generated out of tree, before the real configuration exists), minus
# tests, samples and debugging aids. The big GPU drivers (amdgpu, radeon, nouveau, xe) are modules too, but loading
# one without its firmware blanks the screen (it takes the display over from the firmware framebuffer first), so the
# system loads them only when the firmware package is installed (see gpu-load, a managed file). The graphics the live
# system needs is the firmware's framebuffer (simpledrm) and i915, which is built in.
mkdir -p "$tmp/allcfg"
make O="$tmp/allcfg" ARCH=x86_64 CC="$cc" allmodconfig >/dev/null
grep '^CONFIG_.*=m$' "$tmp/allcfg/.config" | sed -e 's/^CONFIG_//' -e 's/=m$//' |
    grep -v -E '(KUNIT|_TEST$|_TEST_|^TEST_|SELFTEST|SAMPLE|TORTURE|^LKDTM|_DEBUG|^WW_MUTEX)' |
    grep -v -E '^(DRM_VKMS|IKHEADERS)' > "$tmp/allmods.txt"
wc -l < "$tmp/allmods.txt"
kmake defconfig kvm_guest.config
essential_settings

# Many drivers hide behind on/off menu switches the defconfig leaves off (Intel and other sound-firmware
# menus, advanced netfilter, camera/TV/radio media menus, USB4/Thunderbolt). Switch those on; they only
# make options visible, the drivers behind them are still modules.
grep -E '^CONFIG_([A-Z0-9_]+_TOPLEVEL|NETFILTER_ADVANCED|MEDIA_[A-Z_]+_SUPPORT|USB4)=y$' "$tmp/allcfg/.config" >> .config
# nf_tables' address families (inet, ip, ip6, arp, netdev) are yes/no switches: allmodconfig has them on, but the module
# list above holds only modules, and without them no `table inet` can be created, so the firewall cannot load.
# They only appear once NF_TABLES is a module and the advanced menu is visible.
kmake olddefconfig >/dev/null
$c -m NF_TABLES
kmake olddefconfig >/dev/null
for o in NF_TABLES_INET NF_TABLES_IPV4 NF_TABLES_IPV6 NF_TABLES_ARP NF_TABLES_NETDEV; do $c -e $o; done
kmake olddefconfig >/dev/null
# Everything from that list which the base configuration does not already decide, as modules. A module
# that depends on another one the base lacks is dropped by olddefconfig, so repeat: each round enables
# what the previous round made visible.
module_rounds() {
for pass in 1 2 3 4; do
    awk 'NR==FNR { if ($0 ~ /^CONFIG_[A-Za-z0-9_]+=[ym]$/) { sub(/^CONFIG_/, ""); sub(/=.*/, ""); have[$0]=1 } next }
         !($0 in have) { print "CONFIG_" $0 "=m" }' .config "$tmp/allmods.txt" >> .config
    kmake olddefconfig >/dev/null
    echo "round $pass: $(grep -c '^CONFIG_.*=m' .config) modules"
done
kmake olddefconfig
}
module_rounds

# Sound Open Firmware. On Intel laptops from Meteor Lake on (Lunar Lake and later) the audio sits behind a DSP: the plain
# HDA driver cannot find the codec ("Cannot probe codecs, giving up"), there is no sound card at all, and the sound server
# offers only a "Dummy Output". The SOF driver (with the SOF firmware package) is what makes the speakers work. Its menus
# stay hidden until SND_SOC is a module, so they are switched on here, after the rounds above, and the drivers they bring
# in are enabled by another set of rounds.
for o in SND_SOC_SOF_TOPLEVEL SND_SOC_SOF_INTEL_TOPLEVEL; do $c -e $o; done
kmake olddefconfig >/dev/null
for o in SND_SOC_SOF_HDA_LINK SND_SOC_SOF_HDA_AUDIO_CODEC; do $c -e $o; done
$c -m SND_SOC_SOF_PCI -m SND_SOC_SOF_INTEL_SOUNDWIRE
kmake olddefconfig >/dev/null
module_rounds
missing=
for o in SND_SOC_SOF_PCI SND_SOC_SOF_LUNARLAKE SND_SOC_SOF_METEORLAKE SND_SOC_SOF_TIGERLAKE SND_SOC_SOF_ALDERLAKE; do
    grep -q "^CONFIG_$o=m" .config || missing="$missing $o"
done
grep -q '^CONFIG_SND_SOC_SOF_HDA_LINK=y' .config || missing="$missing SND_SOC_SOF_HDA_LINK"
if [ -n "$missing" ]; then echo "sound (SOF) options that did not survive olddefconfig:$missing" >&2; exit 1; fi

# Wi-Fi: every driver the kernel has, as MODULES. (Built-in drivers probe before the root filesystem,
# and so /lib/firmware, is mounted, and would fail to load their firmware; modules load later.)
wifi_gates="WLAN_VENDOR_ADMTEK WLAN_VENDOR_ATH WLAN_VENDOR_ATMEL WLAN_VENDOR_BROADCOM WLAN_VENDOR_CISCO
 WLAN_VENDOR_INTEL WLAN_VENDOR_INTERSIL WLAN_VENDOR_MARVELL WLAN_VENDOR_MEDIATEK WLAN_VENDOR_MICROCHIP
 WLAN_VENDOR_PURELIFI WLAN_VENDOR_QUANTENNA WLAN_VENDOR_RALINK WLAN_VENDOR_REALTEK WLAN_VENDOR_RSI
 WLAN_VENDOR_SILABS WLAN_VENDOR_ST WLAN_VENDOR_TI WLAN_VENDOR_ZYDAS"
wifi_drivers="IWLWIFI IWLDVM IWLMVM
 ATH5K ATH5K_PCI ATH9K ATH9K_PCI ATH9K_HTC ATH10K ATH10K_PCI ATH10K_USB ATH11K ATH11K_PCI ATH12K CARL9170 AR5523
 BRCMSMAC BRCMFMAC BRCMFMAC_SDIO
 RTL8180 RTL8187 RTLWIFI RTL8192CE RTL8192SE RTL8192DE RTL8723AE RTL8723BE RTL8188EE RTL8192EE RTL8821AE RTL8192CU RTL8XXXU
 RTW88 RTW88_PCI RTW88_USB RTW88_8822BE RTW88_8822CE RTW88_8723DE RTW88_8821CE RTW88_8822BU RTW88_8822CU RTW88_8723DU RTW88_8821CU
 RTW89 RTW89_PCI RTW89_8851BE RTW89_8852AE RTW89_8852BE RTW89_8852BTE RTW89_8852CE RTW89_8922AE
 MT7601U MT76x0U MT76x0E MT76x2E MT76x2U MT7603E MT7615E MT7915E MT7921E MT7921U MT7921S MT7996E MT7925E MT7925U
 RT2X00 RT2400PCI RT2500PCI RT61PCI RT2800PCI RT2500USB RT73USB RT2800USB
 MWIFIEX MWIFIEX_PCIE MWIFIEX_USB MWIFIEX_SDIO LIBERTAS LIBERTAS_USB LIBERTAS_SDIO MWL8K
 RSI_91X RSI_USB ZD1211RW AT76C50X_USB"
for o in $wifi_gates; do $c -e $o; done
for o in $wifi_drivers; do $c -m $o; done
# These two are plain yes/no choices inside the Broadcom driver.
for o in BRCMFMAC_USB BRCMFMAC_PCIE; do $c -e $o; done
# zram: the compression algorithms are yes/no switches inside the module (zstd compresses best; swap uses it when present).
for o in ZRAM_BACKEND_ZSTD ZRAM_BACKEND_LZ4 ZRAM_BACKEND_LZO; do $c -e $o; done
fbcon
kmake olddefconfig
check_essential

# Report Wi-Fi drivers this kernel does not have (renamed or removed upstream), without failing the build;
# the ones that matter most must exist, as modules.
nowifi=
for o in $wifi_drivers; do grep -q "^CONFIG_$o=[ym]" .config || nowifi="$nowifi $o"; done
[ -z "$nowifi" ] || echo "note: Wi-Fi options not present in this kernel:$nowifi" >&2
missing=
for o in IWLWIFI IWLMVM ATH9K ATH10K_PCI ATH11K_PCI RTW88_8822CE RTW89_8852AE MT7921E BRCMFMAC RTL8XXXU RT2800USB MWIFIEX_PCIE; do
    grep -q "^CONFIG_$o=m" .config || missing="$missing $o"
done
for o in CFG80211 MAC80211; do grep -q "^CONFIG_$o=y" .config || missing="$missing $o"; done
if [ -n "$missing" ]; then echo "Wi-Fi options that did not survive olddefconfig:$missing" >&2; exit 1; fi
# The firewall needs the nf_tables families, and swap in memory needs zram (see above); the build must not go on without.
missing=
for o in NF_TABLES_INET NF_TABLES_IPV4 NF_TABLES_IPV6 NFT_NAT NFT_CT NFT_MASQ NFT_REDIR NFT_REJECT NFT_LIMIT ZRAM DRM_AMDGPU DRM_RADEON DRM_NOUVEAU DRM_XE; do
    grep -q "^CONFIG_$o=[ym]" .config || missing="$missing $o"
done
if [ -n "$missing" ]; then echo "firewall/swap options that did not survive olddefconfig:$missing" >&2; exit 1; fi
