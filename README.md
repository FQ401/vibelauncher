# VibeLauncher

Minecraft launcher written in Rust for Windows, macOS and Linux: instances, game versions and mod loaders, mods and modpacks, Microsoft account sign-in, Java runtimes. Built-in Vibefork lets players adapt mods and modpacks with their own AI model.

Status: early development.

## Development

`cargo xtask ci` runs the format check, clippy (warnings denied) and tests. `cargo xtask version set X.Y.Z` sets the version in `[workspace.package]`.
