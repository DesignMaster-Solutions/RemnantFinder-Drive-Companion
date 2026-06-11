# Remnant Finder Drive (Companion App)

Desktop companion for Mac and Windows that mounts a virtual drive with all company files.

## Features

- Unified virtual folder tree: Projects, Accounts, Clients, Activities, Company Assets
- Read/write via WebDAV mount (fallback) or native macFUSE / WinFSP when available
- Selective offline folders (pin/unpin)
- Secure token storage in OS keychain
- Auto-start at login (optional)

## Development

### Prerequisites

- **Rust** 1.78+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- **Node.js** 20+
- macOS: Xcode Command Line Tools
- Windows: WebView2; optional [WinFSP](https://winfsp.dev/)

After installing Rust, restart the terminal or run `source "$HOME/.cargo/env"`.

### Icons

App icons are generated from the Remnant Finder brand icon:

```bash
npx tauri icon app-icon-source.png
```

(`app-icon-source.png` is copied from `screenshots/public/app-icon.png`.)

### Run locally

```bash
cd companion
npm install
npm run dev
```

### Build release (macOS → `.app` + `.dmg`)

```bash
npm run build
```

Output:

- `target/release/bundle/macos/Remnant Finder Drive.app`
- `target/release/bundle/dmg/Remnant Finder Drive_1.0.0_aarch64.dmg`

## API

Uses `/api/v1/companies/{company}/drive/*` endpoints. See [companion-drive user guide](../docs/USER_GUIDE/companion-drive.md).

## Mount points

| OS | Default |
|----|---------|
| macOS | `~/Remnant Finder Drive` |
| Windows | `R:` |

WebDAV server runs on `127.0.0.1:17817` when using the WebDAV backend.

Settings are stored in `~/.remnant-finder/config.json` (API URL, auto-mount preference).

### Windows notes

- Drive maps via `net use R: http://127.0.0.1:17817/drive`
- If mount fails, enable the **WebClient** service and allow HTTP WebDAV (local loopback only):
  ```powershell
  sc config WebClient start= auto
  net start WebClient
  reg add HKLM\SYSTEM\CurrentControlSet\Services\WebClient\Parameters /v BasicAuthLevel /t REG_DWORD /d 2 /f
  ```
