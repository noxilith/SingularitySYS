# Building from source

Everything is in <https://github.com/noxilith/SingularitySYS>: a Rust workspace (the `forge` program and its libraries),
package recipes, and scripts. You need a Rust toolchain. Building packages and the ISO also needs a Linux machine with
user namespaces (the build sandbox) and a lot of disk space and time. The design and its history are in
[DESIGN.md](https://github.com/noxilith/SingularitySYS/blob/main/DESIGN.md).

## forge itself

```bash
cargo build                    # target/debug/forge
cargo test --workspace         # the unit and integration tests
scripts/package-forge.sh       # a static forge, packaged and published into a local repository
```

## Packages

A package is a recipe, `recipes/NAME/recipe.toml`: its sources with checksums, the tools it needs at build time, and the
`configure`, `compile` and `install` steps. Builds run in a Linux-namespace sandbox with no network, and only the tools the
recipe names are visible; the tools are themselves packages (the build userland is GNU: bash, coreutils, sed, grep, gawk,
and so on).

```bash
forge build recipes/zlib --libc glibc --tools store --repo repo/glibc-world2 --out packages/out
scripts/build-glibc-world.sh zlib nano htop      # build recipes in order into a repository
```

`scripts/build-glibc-world.sh` uses the final toolchain from a repository (`TOOLCHAIN=final`, `WORLD_REPO=...`,
`WORLD_OUT=...`); the very first toolchain is built once with the host's compiler by `scripts/bootstrap-glibc.sh`.
A rebuilt package that keeps its version needs a revision bump (`1.2_r2`) so the repository picks it; programs record the
exact builds of their libraries, so what links against a rebuilt library is rebuilt as well.

## The repository

`scripts/publish-release.sh` assembles one clean, signed repository from the build outputs and uploads it to the GitHub
release `repo-glibc`. The signing key is `keys/release/singularity-release.key`: back it up, never commit it. Whoever holds
it can publish packages every system will trust. To move to a new key, announce it first (`NEXT_KEYS=...`); if a key leaks,
revoke it (`REVOKE_KEYS=...`): see DESIGN.md, "Revoking a signing key".

## The ISO

```bash
scripts/build-iso.sh           # writes singularity-live.iso
```

An optional boot-menu wallpaper is picked up from `distro/boot/wallpaper.jpg` (it is not in git).

## Testing an image without hardware

QEMU is packaged (`forge install qemu`). `scripts/qemu-boot.sh singularity-live.iso` boots an image headless with the
serial console on your terminal (KVM is used when available). For UEFI and Secure Boot tests use QEMU's secure edk2
firmware (`edk2-x86_64-secure-code.fd` with a copy of `edk2-i386-vars.fd`, machine `q35,smm=on`).
