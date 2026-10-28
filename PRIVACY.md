# Privacy Policy

VibeLauncher is an open-source Minecraft launcher. It runs on the user's computer. The project operates no servers that receive data from the launcher.

Last updated: 2026-10-27.

## Summary

- Account tokens are stored locally in the operating system keyring.
- Nothing is sent to servers operated by the project.
- The launcher has no telemetry, analytics or crash reporting.
- Network requests go only to the third-party services listed below.
- Logs are uploaded only when the user explicitly requests it.

## Data stored on the device

- **Microsoft and Minecraft tokens.** Access and refresh tokens obtained at sign-in are stored in the system keyring (Windows Credential Manager, macOS Keychain, or the Secret Service on Linux). Tokens are not written to configuration files or logs.
- **Account profile.** The Minecraft profile name, UUID and skin reference are stored locally to display the account.
- **Launcher data.** Instances, settings, downloaded game files, mods and logs are stored in the launcher's data directory on the device.
- **Third-party API keys.** A CurseForge API key supplied by the user is stored locally on the device.

Removing an account in the launcher deletes its tokens from the keyring. Uninstalling the launcher and deleting its data directory removes all other local data.

## Network requests

The launcher contacts the following services. Each request is made directly from the user's device and is governed by the privacy policy of that service.

| Service | Purpose | Data sent |
| --- | --- | --- |
| Microsoft identity platform (`login.microsoftonline.com`, `login.live.com`) | Account sign-in | Sign-in flow parameters and refresh tokens; the user enters credentials on Microsoft's own pages |
| Xbox Live (`xboxlive.com`) | Xbox authentication and XSTS authorization | Microsoft access token |
| Mojang / Minecraft services (`minecraftservices.com`, `mojang.com`, `minecraft.net`) | Minecraft sign-in, game ownership check, profile, skins, game version manifests and game files | Minecraft access token, profile and skin requests |
| Modrinth (`modrinth.com`) | Searching and downloading mods and modpacks | Search queries and project identifiers |
| CurseForge (`curseforge.com`, `forgecdn.net`) | Searching and downloading mods and modpacks, identifying installed files | Search queries, project identifiers, fingerprints (32-bit hashes) of mod files, and the user's own CurseForge API key |
| Adoptium (`adoptium.net`) | Downloading Java runtimes | Requested Java version, operating system and architecture |
| mclo.gs | Log sharing | Log content, only when the user chooses to upload a log |

Like any network request, these requests expose the device's IP address to the receiving service.

## Telemetry

The launcher collects no telemetry and does not track usage.

## Data sharing

The project receives no personal data and shares none. Data leaves the device only in the requests listed above.

## Children

The launcher authenticates with Microsoft accounts. Age and parental-consent rules for those accounts are set by Microsoft and Mojang.

## Changes

Changes to this policy are made in this repository and are visible in its history.

## Contact

Questions about privacy: <vibelauncher@vibefork.me>. Issues: <https://github.com/FQ401/vibelauncher/issues>.
