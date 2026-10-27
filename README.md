# VibeLauncher

Minecraft launcher written in Rust for Windows, macOS and Linux: instances, game versions and mod loaders, mods and modpacks, Microsoft account sign-in, Java runtimes. Built-in Vibefork lets players adapt mods and modpacks with their own AI model.

Status: early development.

## Disclaimer

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT. VibeLauncher is an independent project. Minecraft is a trademark of Mojang AB. Microsoft and Xbox are trademarks of Microsoft Corporation. The launcher contains no Minecraft game files; they are downloaded from Mojang's servers.

## Privacy

See [PRIVACY.md](PRIVACY.md).

## Development

`cargo xtask ci` runs the format check, clippy (warnings denied) and tests. `cargo xtask version set X.Y.Z` sets the version in `[workspace.package]`.
