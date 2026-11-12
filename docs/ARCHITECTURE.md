# Architecture

```
┌──────────────────────────┐   ┌──────────────────────────┐
│ apps/desktop (Tauri 2)   │   │ apps/cli (`veranda`)        │
│ ui/ vanilla HTML+JS      │   │ clap sub-commands         │
│ src-tauri commands+tray  │   │ elevated hosts helper     │
└───────────┬──────────────┘   └───────────┬──────────────┘
            └──────────────┬───────────────┘
                 ┌─────────▼──────────┐
                 │ crates/veranda-core  │  pure library, no UI
                 └────────────────────┘
```

## Core modules (`crates/veranda-core/src`)

| Module | Responsibility |
|---|---|
| `paths` | Installation root discovery (`$VERANDA_HOME`, pointer file, `~/veranda`) and directory layout |
| `config` | `etc/veranda.toml` with defaults, validation, unknown keys rejected |
| `manifest` + `packages.toml` | Package kinds, download parts, checksums, built-in verified versions |
| `resolve` | Run-time discovery of newer versions from official APIs/listings |
| `download` | HTTPS-only streaming downloads, checksum verification, TOFU ledger |
| `archive` | Safe zip / tar.gz / tar.xz extraction with strip-components |
| `packages` | Install/remove/registry of runtimes under `bin/<kind>/<version>`; binary lookup |
| `process` | Detached spawn, pid files, graceful stop, port checks |
| `services/*` | Web (Caddy/Nginx/Apache), PHP, DB, cache, mail: prepare config, start, stop, status |
| `templates/*` | Hardened configuration templates rendered with `{{VAR}}` substitution |
| `sites` | Auto-discovered project folders + manual sites (`etc/sites.toml`) |
| `hosts` | Managed block in the system hosts file, payload validation, elevated apply |
| `tls` | Local CA + site certificate (rcgen), trust-store commands |
| `db` | Init data dir, root password, SQL helpers, dumps |
| `projects` | Blank / WordPress / Laravel / Symfony / Composer / git templates |
| `backup` | Periodic dumps with rotation |
| `update` | GitHub release check for the app itself |
| `tools` | Open URL/folder/editor/terminal, elevation (pkexec / osascript / UAC) |

## Installation root

```
~/veranda
├── bin/<kind>/<version>/   runtimes (php/8.5.8, caddy/2.11.7, mariadb/13.0.2 ...)
├── etc/veranda.toml          configuration
├── etc/sites.toml          manual sites
├── etc/caddy|nginx|apache  generated web server config
├── etc/php/php.ini         generated; etc/php/conf.d/99-user.ini is yours
├── etc/mariadb/my.cnf      generated
├── etc/ssl/ca.crt ca.key   local CA (key 0600)
├── etc/ssl/sites.crt/key   leaf certificate covering every site
├── etc/secrets/db-root.txt database root password (0600)
├── etc/apps/               adminer, phpmyadmin (served at /adminer, /phpmyadmin on localhost)
├── data/<engine>/          database files
├── www/                    document root: each folder -> https://<folder>.test
├── logs/                   one log per service + server logs
├── run/                    pid files
├── tmp/                    downloads, sessions, upload temp
└── backups/                SQL dumps
```

## Service start order

`db -> cache -> mail -> php -> web`. The web server config is regenerated before every start or
reload from the current site list, so adding a folder and pressing *Sync* is all it takes.

## Versions and updates

- Built-in manifest: verified versions + checksums at release time (tests enforce https URLs and
  coverage for Linux x86_64, Windows x86_64, macOS arm64).
- `veranda packages --check` / *Check latest versions* queries upstream sources; the built-in list is
  the fallback when offline.
- App updates: `update.repo = "owner/repo"` in the config; the CLI and the desktop app compare the
  newest GitHub release tag with the running version. Releases are produced by
  `.github/workflows/release.yml` on tag push.
