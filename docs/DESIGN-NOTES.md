# Design notes

Purpose: record which behaviours of existing local-stack tools (Laragon, XAMPP, Laravel Herd,
Valet and similar) Veranda reproduces, and where it deliberately differs. Veranda is a clean-room,
original implementation: no code, templates, translations, icons or other files from any of
those products are included in this repository. Product names are used only for identification
and belong to their respective owners.

## Typical shape of a bundled local stack (observed from public installers and documentation)
- `laragon.exe` (Delphi GUI), `bin/laragon/` helper utilities (curl, openssl, sqlite3, signal
  senders, a Node script for hosts editing), language files, config templates.
- Bundled runtimes: Apache (Apache Lounge build), PHP 8.3 NTS, MySQL 8.4, Nginx, Redis 5 for
  Windows, Memcached, Mailpit, Node 22, Python 3.13, Git, Composer, Notepad++, HeidiSQL, Cmder, ngrok.
- `etc/apache2`, `etc/nginx` (alias configs for phpMyAdmin/Adminer, SSL config, fcgid), `etc/apps`.
- `usr/packages.conf`: a list of `name=url` download entries (PHP, Apache, Nginx, Node, phpMyAdmin,
  MySQL, PostgreSQL, Go, PocketBase ...) used by its "quick add" feature.
- `usr/profile/default.ini`: the selected version per runtime.

## Behaviours users expect (and how Veranda provides them)

| Common behaviour | Veranda |
|---|---|
| Folder in `www/` -> `name.test` vhost + hosts entry | `sites::discover` + hosts managed block |
| `{name}.test` hostname format, configurable TLD | `general.hostname_format`, `general.tld` |
| Apache `VirtualHost` with `ServerAlias *.name.test` | Caddy/Nginx/Apache blocks include `*.<host>` |
| SSL per site via OpenSSL, "add cert to trust store" | rcgen CA + single multi-SAN leaf; `veranda ssl trust` |
| Services: Apache/Nginx, MySQL, PHP, Redis, Memcached, Mailpit | Web (Caddy default), DB, PHP, cache (Valkey/Redis), Mailpit |
| Multiple PHP/Apache versions under `bin/<kind>/<version>` | same layout, `veranda use <kind> <version>` |
| `packages.conf` quick downloads | manifest + run-time resolvers with checksums |
| Quick create: Blank, WordPress, Laravel, Symfony, CakePHP | Blank, WordPress, Laravel, Symfony, any Composer template, git |
| Auto-create database for new project | `--db` / "create database" checkbox |
| Root password change, reset, copy | `veranda db set-password`, random by default |
| Backup data dir every 8 h, keep 5 | SQL dumps every 8 h, keep 5 (consistent, engine-agnostic) |
| "Add to PATH", terminal with runtimes on PATH | `veranda terminal`, PATH prefix per installed runtime |
| Procfile for user programs | out of scope for 0.1 (planned: `usr/services.toml`) |
| Mail catcher window, Gmail sender | Mailpit UI; PHP `sendmail_path` -> Mailpit |
| ngrok sharing, QR code | out of scope (third-party account) |
| Run at Windows startup, minimized to tray | `general.auto_start`, `general.start_minimized`; OS autostart left to the user |
| 33 UI languages | English + Indonesian in 0.1; dictionary-based, easy to extend |

## Common weak defaults in this tool category (and Veranda's choice)
- Empty MySQL root password by default; Veranda generates a random one.
- Nginx SSL templates allowed TLSv1/1.1 and weak ciphers (`RC4`, `EXP`, `LOW`); Veranda uses TLS 1.2/1.3
  and AEAD suites only.
- `secure-file-priv=""` (disabled); Veranda restricts it to the tmp directory.
- Hosts file rewritten by a Node script that copies an entire temp file; Veranda validates each entry.
- Downloads from `packages.conf` had no checksum verification; Veranda verifies or pins on first use.
- Runtimes bundled in the installer age quickly; Veranda downloads current versions on demand.
