# Roblox Account Manager

A lightweight Windows app for managing multiple Roblox accounts, written in Rust.

## Features

- Encrypted account storage (Windows DPAPI, or AES-256-GCM with a password)
- Add accounts by cookie, username/password, or bulk import
- Launch into a game, a specific server, a private server, or the Roblox home page
- Run multiple Roblox clients at the same time
- Server browser with player count, ping and FPS
- Auto-rejoin when a client closes or disconnects
- Anti-AFK

## Building

Requires Rust (stable, `x86_64-pc-windows-msvc`).

```
cargo build --release
```

The binary is written to `target\release\roblox-account-manager-rs.exe`.

## Usage

1. Start the app before opening Roblox. Multi-instance only works if the app starts first; the status in the top-right corner shows `mutex held` when it's active. If Roblox was already open, close it and the app picks it up automatically.
2. Add accounts with **Add account** on the Accounts tab.
3. Select one or more accounts (click, Ctrl+click, or checkboxes), choose a target on the right, and press **Launch**.
4. Use **Server browser** to pick a specific server for the selected accounts.
5. **Sessions** lists running clients. Clients that close unexpectedly are rejoined, to the same server when possible.
6. Retry limits, anti-AFK, UI scale and password protection are under **Settings**.

### handle64 mode

Optionally, the app can also close Roblox's singleton handles using Sysinternals `handle64.exe`. This requires running as administrator. Place `handle64.exe` in a `utils\` folder next to the exe, or download it from Settings.

## Notes

- Username/password login opens the Roblox login page in Chrome, Edge or Brave (temporary profile) with your details filled in. Solve any captcha or 2-step check there; the account is added and the window closes once you're logged in. **Log in without browser** tries the login API first and falls back to the browser if Roblox asks for a captcha.
- Roblox's public server list does not include server regions.
- Anti-AFK has two modes: *Background* sends key presses directly to the Roblox window without changing focus, and *Focus* briefly focuses each window. Use Focus if Background doesn't keep you from being kicked.

## Data

Stored in `%APPDATA%\RobloxAccountManagerRS\`:

| File | Contents |
|---|---|
| `accounts.dat` | Encrypted accounts |
| `settings.json` | Settings (no secrets) |
| `handle64.exe` | Only if downloaded from Settings |

## Credits

- Multi-instance approach based on [evanovar/RobloxAccountManager](https://github.com/evanovar/RobloxAccountManager).
- Icon based on the icon of [ic3w0lf22/Roblox-Account-Manager](https://github.com/ic3w0lf22/Roblox-Account-Manager) (GPL-3.0). Regenerate it with `powershell -File assets\make-icon.ps1`.
