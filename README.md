# Roblox Account Manager

A small Windows app, written in Rust, for running several Roblox accounts side by side.

## Features

- Accounts are stored encrypted with Windows DPAPI, or with AES-256-GCM if you set a password
- Add accounts by logging in, by cookie, or in bulk
- Launch into a game, a specific server, a private server or the Roblox home screen
- Run several Roblox clients at once
- Recent games, account groups and group order
- Open roblox.com in a window logged in as an account
- Server browser with player count, ping and FPS, plus a search for the server a player is in
- Universe tab that lists every place in a game, with game search by name
- Expired cookies are flagged in the account list
- Crashed or disconnected clients rejoin on their own
- Anti-AFK
- Export and import of accounts and settings in one password-protected file
- Themes, including a 2011 Roblox look

## Building

Requires Rust (stable, `x86_64-pc-windows-msvc`).

```
cargo build --release
```

The exe ends up at `target\release\roblox-account-manager-rs.exe`.

## Usage

1. Start the app before Roblox, since multi-instance depends on it. The status in the top right reads `mutex held` when it's working.
2. Add accounts with Add account, using a username, a cookie or a bulk list.
3. Select accounts (click, Ctrl+click, Shift+click), enter a place and press Launch. Home starts Roblox on its home screen and Browser opens roblox.com as that account.
4. Right-click accounts to put them in groups and change the group order.
5. Use the Server browser to join a specific server. Type a username under Find player to locate their server, or press Join to go straight there with the selected accounts.
6. Use the Universe tab to list all places in a game, or to search for a game by name.
7. Sessions lists the clients you launched. A crashed client rejoins, to the same server when it can.
8. Retries, anti-AFK, theme, password protection and export/import are in Settings.

### handle64 mode

The app can also close Roblox's singleton handles with Sysinternals `handle64.exe`, which needs administrator rights. Put `handle64.exe` in a `utils\` folder next to the exe, or download it from Settings.

## Notes

- Logging in with a username opens Roblox's login page in a WebView2 window (the Edge engine built into Windows) on a temporary profile. Press Log in and get through any captcha or 2-step check, and the window closes by itself once you're in. Without WebView2 it falls back to Helium, Chrome, Edge or Brave.
- Log in without browser tries Roblox's login API first and switches to the browser if a captcha comes up.
- Logging out in a browser kills that account's saved cookie, so Browser windows block the logout button.
- Find player first checks the player's online status. If that hides their server, it compares avatars across the place's public servers, which needs one of your accounts because Roblox only lists players to logged-in users.
- Roblox's public server list has no regions.
- Anti-AFK has two modes. Background sends keys without changing focus, and Focus briefly brings each window to the front. Switch to Focus if Background doesn't stop the kicks.

## Data

Everything lives in `%APPDATA%\RobloxAccountManagerRS\`:

| File | Contents |
|---|---|
| `accounts.dat` | Encrypted accounts |
| `settings.json` | Settings (no secrets) |
| `handle64.exe` | Only if downloaded from Settings |

Exports are `.ramrs` files, saved to Downloads by default.

## Credits

- Multi-instance approach based on [evanovar/RobloxAccountManager](https://github.com/evanovar/RobloxAccountManager).
- Icon based on [ic3w0lf22/Roblox-Account-Manager](https://github.com/ic3w0lf22/Roblox-Account-Manager) (GPL-3.0). Regenerate it with `powershell -File assets\make-icon.ps1`.
