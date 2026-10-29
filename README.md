# VibeLauncher

<img src="assets/icons/mark.svg" alt="VibeLauncher logo" width="96" align="right">

Minecraft launcher written in Rust for Windows, macOS and Linux: instances, game versions and mod loaders, mods and modpacks, Microsoft account sign-in, Java runtimes. Built-in Vibefork lets players adapt mods and modpacks with their own AI model.

Status: early development.

## Disclaimer

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT. VibeLauncher is an independent project. Minecraft is a trademark of Mojang AB. Microsoft and Xbox are trademarks of Microsoft Corporation. The launcher contains no Minecraft game files; they are downloaded from Mojang's servers. VibeLauncher is published by the VibeLauncher contributors; contact: <vibelauncher@vibefork.me>.

## Privacy

See [PRIVACY.md](PRIVACY.md).

## Installing on Windows

The Windows release comes as an installer, `vibelauncher-<version>-windows-x64.msi`, and as a portable archive, `vibelauncher-<version>-windows-x64-portable.zip`. The installer needs no administrator rights: it installs VibeLauncher for the current user into `%LOCALAPPDATA%\Programs\VibeLauncher` and adds it to the Start menu; Settings > Apps removes it. The portable archive runs from any folder and keeps its data in `data` next to the executables.

The releases are not signed with a code signing certificate, so Microsoft Defender SmartScreen may stop the first start of a downloaded file with "Windows protected your PC". Choose More info, then Run anyway. Updates that VibeLauncher installs itself are not marked as downloaded from the internet.

## Installing on macOS

The macOS release is one disk image for Apple silicon and Intel Macs, macOS 11 (Big Sur) or newer: `vibelauncher-<version>-macos-universal.dmg`. Open it and drag VibeLauncher into Applications.

VibeLauncher is signed ad hoc and not notarized by Apple, so Gatekeeper blocks the first start of the downloaded app:

1. Open VibeLauncher from Applications. macOS reports that it cannot verify the app; choose Done.
2. Open System Settings > Privacy & Security, scroll to Security and choose Open Anyway next to the message about VibeLauncher. Confirm with your password.

On macOS 14 (Sonoma) and older, Control-click the app in Finder, choose Open and confirm instead. Alternatively, `xattr -dr com.apple.quarantine /Applications/VibeLauncher.app` in Terminal removes the download mark, after which the app opens without asking. The choice is asked once per download: updates that VibeLauncher installs itself are not marked as downloaded and open without asking.

## Development

The workspace layout, layers and the core facade are described in [docs/architecture.md](docs/architecture.md). `cargo run -p vl-cli -- --mock instances list` runs the command line on synthetic data.

`cargo xtask ci` runs the layer check, the dependency source check, the format check, clippy (warnings denied), tests and a WebAssembly build of the core layer. `cargo xtask version set X.Y.Z` sets the version in `[workspace.package]`. `cargo xtask package portable` builds the portable Windows archive into `target/dist`; `cargo xtask package msi` builds the Windows installer (it needs the .NET SDK for the pinned WiX 7 tool); `cargo xtask package linux` builds the Linux tar.gz, AppImage, deb and rpm packages (it needs appimagetool and nfpm, see [docs/architecture.md](docs/architecture.md#packaging)); `cargo xtask package macos` builds the universal macOS disk image on a Mac. The `release` workflow builds every package and drafts a GitHub release with `SHA256SUMS.txt` and the self-update manifest `latest.json` (see [docs/architecture.md](docs/architecture.md#packaging)).
