# VibeLauncher

Minecraft launcher written in Rust for Windows, macOS and Linux: instances, game versions and mod loaders, mods and modpacks, Microsoft account sign-in, Java runtimes. Built-in Vibefork lets players adapt mods and modpacks with their own AI model.

Status: early development.

## Disclaimer

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT. VibeLauncher is an independent project. Minecraft is a trademark of Mojang AB. Microsoft and Xbox are trademarks of Microsoft Corporation. The launcher contains no Minecraft game files; they are downloaded from Mojang's servers. VibeLauncher is published by the VibeLauncher contributors; contact: <vibelauncher@vibefork.me>.

## Privacy

See [PRIVACY.md](PRIVACY.md).

## Installing on macOS

The macOS release is one disk image for Apple silicon and Intel Macs, macOS 11 (Big Sur) or newer: `vibelauncher-<version>-macos-universal.dmg`. Open it and drag VibeLauncher into Applications.

VibeLauncher is signed ad hoc and not notarized by Apple, so Gatekeeper blocks the first start of the downloaded app:

1. Open VibeLauncher from Applications. macOS reports that it cannot verify the app; choose Done.
2. Open System Settings > Privacy & Security, scroll to Security and choose Open Anyway next to the message about VibeLauncher. Confirm with your password.

On macOS 14 (Sonoma) and older, Control-click the app in Finder, choose Open and confirm instead. Alternatively, `xattr -dr com.apple.quarantine /Applications/VibeLauncher.app` in Terminal removes the download mark, after which the app opens without asking. The choice is asked once per download: updates that VibeLauncher installs itself are not marked as downloaded and open without asking.

## Development

The workspace layout, layers and the core facade are described in [docs/architecture.md](docs/architecture.md). `cargo run -p vl-cli -- --mock instances list` runs the command line on synthetic data.

`cargo xtask ci` runs the layer check, the format check, clippy (warnings denied), tests and a WebAssembly build of the core layer. `cargo xtask version set X.Y.Z` sets the version in `[workspace.package]`. `cargo xtask package portable` builds the portable Windows archive into `target/dist`; `cargo xtask package linux` builds the Linux tar.gz, AppImage, deb and rpm packages (it needs appimagetool and nfpm, see [docs/architecture.md](docs/architecture.md#packaging)); `cargo xtask package macos` builds the universal macOS disk image on a Mac.
