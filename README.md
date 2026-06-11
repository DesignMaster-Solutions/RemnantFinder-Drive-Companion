# Stone Project Drive (Companion App)

Desktop companion for Mac and Windows that mounts a virtual drive with all company files.

## Quick start

```bash
cd companion
npm install
npm run dev      # development
npm run build    # release .app / .dmg / .exe
```

## Release channel

Installers are published to the public GitHub repo **[StoneProject-CompanionDrive](https://github.com/DesignMaster-Solutions/StoneProject-CompanionDrive)** (installers only, no source code).

| Platform | Asset |
|----------|-------|
| macOS | `StoneProjectDrive-{version}.dmg` |
| Windows | `StoneProjectDrive-Setup.exe` |

The main monorepo workflow `.github/workflows/companion-release.yml` builds on tag `companion-v*`.

## Mount points

| OS | Default path |
|----|--------------|
| macOS | `~/Stone Project Drive` |
| Windows | `R:` (WebDAV via WebClient) |

## API

Uses `/api/v1/companies/{company}/drive/*` endpoints. Version/download: `/api/v1/companion/version` and `/api/v1/companion/download` (redirects to GitHub Releases).

See [companion-drive user guide](../docs/USER_GUIDE/companion-drive.md).
