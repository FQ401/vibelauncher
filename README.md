# VibeLauncher

Minecraft launcher written in Rust for Windows, macOS and Linux: instances, game versions and mod loaders, mods and modpacks, Microsoft account sign-in, Java runtimes. Built-in Vibefork lets players adapt mods and modpacks with their own AI model.

Status: early development.

## Disclaimer

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT. VibeLauncher is an independent project. Minecraft is a trademark of Mojang AB. Microsoft and Xbox are trademarks of Microsoft Corporation. The launcher contains no Minecraft game files; they are downloaded from Mojang's servers. VibeLauncher is published by the VibeLauncher contributors; contact: <vibelauncher@vibefork.me>.

## Privacy

See [PRIVACY.md](PRIVACY.md).

## Development

The workspace layout, layers and the core facade are described in [docs/architecture.md](docs/architecture.md). `cargo run -p vl-cli -- --mock instances list` runs the command line on synthetic data.

`cargo xtask ci` runs the layer check, the format check, clippy (warnings denied), tests and a WebAssembly build of the core layer. `cargo xtask version set X.Y.Z` sets the version in `[workspace.package]`. `cargo xtask package portable` builds the portable Windows archive into `target/dist`.
