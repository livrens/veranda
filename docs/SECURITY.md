# Security model

Veranda runs third-party servers on a developer machine. The goals are: nothing reachable from the
network, nothing tampered with in transit, no silent privilege escalation, and sane defaults even
if the user never opens the settings.

## Network exposure
- Every service listens on `127.0.0.1` / `::1` only (web, PHP FastCGI, database, cache, SMTP,
  Mailpit UI). Nginx/Apache/Caddy templates bind the loopback address explicitly.
- Caddy runs with the admin API **disabled** (`admin off`); reloads are restarts.
- Valkey/Redis: `protected-mode yes`, loopback bind.
- MariaDB/MySQL: `bind-address = 127.0.0.1`, `local-infile = 0`, `secure-file-priv` restricted to
  the tmp dir, `mysqlx` off, random 24-character root password generated on first start and stored
  with mode 0600 (Windows: ACL restricted to the user). Passwords are passed via `MYSQL_PWD`, never
  on the command line.

## Supply chain
- Downloads are HTTPS only (`https_only(true)` in the HTTP client, rustls, certificate validation).
- Each package carries a checksum: SHA-256/SHA-512 where upstream publishes one, remote checksum
  files (Caddy, Node.js, Composer, phpMyAdmin, Apache Lounge, Valkey), MD5 where that is all the
  vendor offers (MySQL). Sources with no checksum at all (static-php builds, nginx.org zips) fall
  back to **trust-on-first-use**: the SHA-256 is recorded in `etc/checksums.json`, and any later
  download of the same URL with different content is refused.
- Archives are extracted with `enclosed_name()` / component validation (no `..`, no absolute paths),
  symlinks that escape the destination are skipped. Downloads are capped at 2 GiB.
- Nothing downloaded is ever executed during installation except the vendor binaries the user
  explicitly chose to run as a service.

## Privilege boundaries
- The app never runs as root/administrator. Only two operations need elevation and both go
  through a minimal helper:
  1. **Hosts file** - the GUI/CLI writes a payload (`tld=` + validated hostnames) and runs
     `veranda hosts apply --from <payload>` elevated (pkexec / osascript / UAC). The helper
     re-validates every hostname against a strict grammar, only touches the lines between the Veranda
     markers, keeps a backup and writes in place.
  2. **Ports < 1024 on Linux** - `veranda doctor --fix-ports` applies `cap_net_bind_service` to the web
     server binary with `setcap` (optional; ports 8080/8443 work without it).
- CA trust is installed into the **current user's** store (NSS db on Linux, login keychain on
  macOS, `certutil -user` on Windows). System-wide installation on Linux is a separate, explicit
  elevated step.

## TLS
- Local CA: ECDSA P-256, 10 years, path length 0, key 0600, never leaves the machine.
- Site certificate: one leaf covering `localhost`, `127.0.0.1`, every `<site>` and `*.<site>`,
  validity 820 days (under the 825-day browser limit), re-issued when the site set changes.
- Servers: TLS 1.2 and 1.3 only, modern AEAD cipher suites, HTTP/2.

## Web server hardening
- Dot-files (`.git`, `.env`, `.htaccess`) and dependency manifests return 404.
- `X-Content-Type-Options: nosniff`, `Server` header removed, directory listing is a setting.
- PHP: `expose_php=Off`, `allow_url_include=Off`, `cgi.fix_pathinfo=0`, strict sessions with
  HttpOnly/SameSite cookies, `open_basedir` left open on purpose for development.
- Tool apps (Adminer, phpMyAdmin) are only mounted on the `localhost` site, never on project hosts.

## Desktop app
- Tauri 2 with a strict CSP, no remote content, IPC limited by a capability file to the commands
  the UI needs. No bundled browser engine (system WebView), no telemetry, no auto-executed
  downloads.
- **App lock** (Settings > Security): a PIN (digits) or a password asked for at launch and
  after idle time, with an optional second factor: a 6-digit TOTP code (RFC 6238, SHA-1, 30 s)
  from an authenticator app. The TOTP secret is stored in the same private file (it has to be
  readable to verify codes); codes are accepted once (no replay) within one step of drift. Stored as a salted PBKDF2-HMAC-SHA256 hash (600,000 rounds, constant-time verify) in
  `etc/secrets/app-lock.json`, readable by the current user only; the secret itself is never
  written. While locked, every IPC command that reads or changes state is refused by the backend
  (not only hidden by the UI), and unlock attempts slow down after five failures. Scope: it
  protects the Veranda window and its commands. It does not encrypt `www/`, databases or
  secrets on disk - on a lost device that is the job of full-disk encryption (BitLocker,
  FileVault, LUKS) plus the operating-system login; the lock complements them.
- Opening URLs and folders on Windows goes through `rundll32 url.dll,FileProtocolHandler` and
  `explorer.exe` with the target as one argument, never through `cmd /C start`, whose parser
  would treat `&`, `^` or `%` inside a URL or folder name as commands. URLs with whitespace or
  quotes are refused before that.

## Dependencies
- Rust 2024 edition; every crate pinned to the newest stable release at the time of writing
  (see `Cargo.toml`). `cargo audit` runs in CI. Prefer updating dependencies with every release.

## Remote (SSH client)
- Pure-Rust SSH (`russh` + `ring`), no shelling out to `ssh`. Host keys are trust-on-first-use with
  the SHA-256 fingerprint shown before anything is sent; a changed key is refused unless the user
  explicitly accepts it, and the old line is then replaced in `etc/remote/known_hosts`.
- Private keys are written with mode 0600 under `etc/remote/keys/`; passphrases are optional and
  use OpenSSH's bcrypt/aes256-ctr format. Export of a private key is an explicit user action
  behind a confirmation.
- Passwords and passphrases go to the OS credential store (Keychain / Credential Manager /
  Secret Service). If none is available they are kept in `secrets/remote-secrets.json` (0600) and
  the Keychain page says so. They are never written to the host store or logs.
- Forwarding listeners bind `127.0.0.1` by default; the SOCKS5 proxy has no authentication and is
  meant for loopback use only. Remote forwards only reach the destination the rule names.
- "Install key on host" appends one validated public-key line to `~/.ssh/authorized_keys` with
  `umask 077`; the public key text is rejected if it contains quotes or newlines.
- Veranda never reads other SSH clients' databases; "Import" parses `~/.ssh/config` (and PuTTY
  registry sessions on Windows) only when clicked.
- Known advisory, accepted: RUSTSEC-2023-0071 (`rsa` crate, Marvin timing side channel, no fix
  released). It only affects RSA keys, used by the SSH client to sign a login challenge locally;
  there is no RSA decryption exposed to the network. Generated keys are Ed25519 by default. The
  entry is listed in `.cargo/audit.toml` and should be removed once a fixed `rsa` ships.

## Where your data goes (review of 2026-11-06)

What was checked: every outbound request in the code, every file that holds a secret, every
log line, every listening socket, and the crash-reporting path.

- **Network.** Veranda contacts only the vendors' download servers (nodejs.org, php.net,
  static-php.dev, mariadb.org, dl.mysql.com, go.dev, GitHub releases, Adoptium, Microsoft,
  ziglang.org, Maven Central, mongodb.org, valkey.io, curl.se, getcomposer.org, phpmyadmin.net,
  apachelounge.com, nginx.org) and the GitHub API for version lists and update checks. All of it
  over HTTPS with checksum verification. There is no telemetry, analytics, crash upload or
  account; bundled services are started with their telemetry switched off (Meilisearch, Qdrant).
  SSH connections go only to the hosts you add. A Cloudflare quick tunnel is started only when you
  click Share and confirm the dialog; it exposes that one site until you stop it.
- **Storage.** Everything lives under the Veranda root. Secrets (database root password, SSH
  passwords/passphrases when no OS credential store exists, private keys, the local CA key) are
  written with user-only permissions (0600 / restricted ACL). The remote host list carries flags
  such as `has_password`, never the password. Exports of private keys are explicit actions.
- **Logs.** No password, passphrase, token or key is ever logged or printed; database passwords
  are passed to client tools through the environment, not the command line.
- **Listeners.** Every service binds 127.0.0.1 only; forwarding rules default to 127.0.0.1 and a
  non-loopback bind asks for confirmation.
- **Process hardening.** On Linux/macOS the app disables core dumps for itself and every helper it
  starts (WebKit, services) and marks itself non-dumpable, so crash reporters such as Ubuntu's
  Apport cannot upload a memory image that might contain a password being typed
  (`VERANDA_ALLOW_COREDUMP=1` re-enables dumps for debugging).
- **Updates.** Installers are verified against Veranda's updater public key before they are
  applied; the key pair is separate from code signing (see `SIGNING.md`).
