# Roblox Account Manager

Lightweight Windows app for managing multiple Roblox accounts, written in Rust.

## Features

- Encrypted account storage (Windows DPAPI, or AES-256-GCM with a password)
- Add accounts by cookie, username/password or bulk import
- Launch into a game, server, private server or Roblox home
- Multiple Roblox clients at once
- Recent games and group priority
- Open roblox.com in a browser logged in as an account
- Server browser with player count, ping and FPS
- Auto-rejoin when a client crashes or disconnects
- Anti-AFK
- Themes, including a classic 2011 Roblox look

## Building

Requires Rust (stable, `x86_64-pc-windows-msvc`).

```
cargo build --release
```

Output: `target\release\roblox-account-manager-rs.exe`.

## Usage

1. Start the app before Roblox. Multi-instance needs this. The top-right status shows `mutex held` when active.
2. Add accounts with **Add account**.
3. Select accounts (click, Ctrl+click, Shift+click), enter a place and press **Launch**. **Home** opens the Roblox home screen, **Browser** opens roblox.com.
4. Right-click accounts to set groups and group order.
5. **Server browser** picks a specific server.
6. **Sessions** lists running clients. Crashed clients rejoin, to the same server when possible.
7. Retries, anti-AFK, theme and password protection are under **Settings**.

### handle64 mode

The app can also close Roblox's singleton handles with Sysinternals `handle64.exe`. Requires administrator. Put `handle64.exe` in a `utils\` folder next to the exe, or download it from Settings.

## Notes

- Username/password login opens Roblox's login page in Helium, Chrome, Edge or Brave on a temporary profile. Press **Log in** and solve any captcha or 2-step check. The window closes once you're logged in.
- **Log in without browser** tries the login API first and falls back to the browser on a captcha.
- Logging out in a browser invalidates that account's cookie. Close the window instead.
- Roblox's public server list has no regions.
- Anti-AFK *Background* sends keys without changing focus. *Focus* briefly focuses each window. Use Focus if Background doesn't prevent kicks.

## Data

Stored in `%APPDATA%\RobloxAccountManagerRS\`:

| File | Contents |
|---|---|
| `accounts.dat` | Encrypted accounts |
| `settings.json` | Settings (no secrets) |
| `handle64.exe` | Only if downloaded from Settings |

## Credits

- Multi-instance approach based on [evanovar/RobloxAccountManager](https://github.com/evanovar/RobloxAccountManager).
- Icon based on [ic3w0lf22/Roblox-Account-Manager](https://github.com/ic3w0lf22/Roblox-Account-Manager) (GPL-3.0). Regenerate with `powershell -File assets\make-icon.ps1`.
