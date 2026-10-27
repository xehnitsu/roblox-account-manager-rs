# Roblox Account Manager

A Windows app, written in Rust, for running several Roblox accounts side by side.

## Features

- Accounts stored encrypted with Windows DPAPI, or AES-256-GCM if you set a password
- Add accounts by login, cookie, or in bulk
- Launch into a game, a specific server, a private server, or the Roblox home screen
- Multiple Roblox clients at once
- Recent games, account groups, and group order
- roblox.com in a window logged in as an account
- Server browser with player count, ping and FPS, plus a search for the server a player is in
- Universe tab listing every place in a game, with game search by name
- Expired cookies flagged in the account list
- Crashed or disconnected clients rejoin automatically
- Anti-AFK
- Export and import of accounts and settings in one password-protected file
- Themes, including a 2011 Roblox look

## Building

Requires stable Rust (`x86_64-pc-windows-msvc`).

```
cargo build --release
```

The exe is `target\release\roblox-account-manager-rs.exe`.

## Usage

1. Start the app before Roblox. Multi-instance depends on it, and the status in the top right reads `mutex held` when it works.
2. Add accounts with Add account (username, cookie, or bulk list).
3. Select accounts (click, Ctrl+click, Shift+click), enter a place, and press Launch. Home opens the Roblox home screen and Browser opens roblox.com as that account.
4. Right-click accounts to set groups and group order.
5. Use the Server browser to join a specific server. Enter a username under Find player to locate their server, or press Join to go straight there with the selected accounts.
6. Use the Universe tab to list all places in a game or search for a game by name.
7. Sessions lists the clients you launched. A crashed client rejoins, to the same server when possible.
8. Retries, anti-AFK, theme, password protection, and export/import are in Settings.

### handle64 mode

The app can also close Roblox's singleton handles with Sysinternals `handle64.exe`, which needs administrator rights. Put `handle64.exe` in a `utils\` folder next to the exe, or download it from Settings.

## Notes

- Logging in with a username opens Roblox's login page in a WebView2 window (the Edge engine built into Windows) on a temporary profile. Press Log in and complete any captcha or 2-step check. The window closes once you're in. Without WebView2 it falls back to Helium, Chrome, Edge, or Brave.
- Log in without browser tries Roblox's login API first and switches to the browser if a captcha comes up.
- Logging out in a browser invalidates that account's saved cookie, so Browser windows block the logout button.
- Find player first checks the player's online status. If that hides their server, it compares avatars across the place's public servers. This needs one of your accounts, because Roblox only lists players to logged-in users.
- Roblox's public server list has no regions.
- Anti-AFK has two modes. Background sends keys without changing focus. Focus briefly brings each window to the front. Use Focus if Background doesn't stop the kicks.

## Data

Everything is in `%APPDATA%\RobloxAccountManagerRS\`:

| File | Contents |
|---|---|
| `accounts.dat` | Encrypted accounts |
| `settings.json` | Settings (no secrets) |
| `handle64.exe` | Only if downloaded from Settings |

Exports are `.ramrs` files, saved to Downloads by default.

## Credits

- Multi-instance approach based on [evanovar/RobloxAccountManager](https://github.com/evanovar/RobloxAccountManager).

## License

MIT, see [LICENSE](LICENSE).
