# Changelog

All notable changes to Veranda are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

Release checklist: bump `VERSION`, `workspace.package.version` in `Cargo.toml` and `version` in `apps/desktop/src-tauri/tauri.conf.json`, move the
Unreleased notes under a new heading, tag `vX.Y.Z` and push the tag - the release workflow builds
installers for Linux, Windows and macOS and attaches them to the GitHub release.

## [Unreleased]

## [1.0.11] - 2026-11-12

### Fixed
- Terminal: the view is sized again whenever the stats strip, search row or window changes the
  space, so the bottom lines (the prompt) are no longer hidden below the edge; new output keeps
  the view at the bottom unless you scrolled up on purpose.
- Terminal stats refresh every 5 s; RAM shows its percentage and Disk shows the all-disks figure
  next to the fullest mount, so the numbers line up with cloud consoles.
- Updates: a config that still names an earlier home of the project (`forallgame/padu`,
  `forallgame/veranda`, `livrensandi1/veranda`) in `[update] repo` is treated like an empty
  setting, so the check goes to the current repository.

## [1.0.10] - 2026-11-12

### Changed
- README (English and Indonesian) and the user guide describe the current features: app lock
  with two-factor, looks, terminal server stats, MariaDB ↔ MySQL migration with pause/cancel;
  install examples use current file names; .deb/.rpm installs update in place.
- CI and Release workflows use `actions/checkout@v5` (Node 24 runners, no deprecation notice).

## [1.0.9] - 2026-11-12

### Changed
- The project now lives at https://github.com/livrens/veranda. Updates are checked there from
  this version on; installs that still point at the old address receive this release once and
  then follow the new one.

## [1.0.8] - 2026-11-11

### Added
- Migrate: **Pause/Resume** and **Cancel**. Cancel stops after the database being copied (nothing
  half done); a second press, "Stop now", ends the current copy and marks that database as
  copied again next time. Pause holds the stream between dump and client, so resuming loses
  nothing.

### Fixed
- Migrate button shows a spinner while a migration runs instead of a spinning arrow.

## [1.0.7] - 2026-11-11

### Fixed
- MySQL 8/9: every Veranda client call (start-up check, shutdown, backups, migration import)
  now passes `--get-server-public-key`, so `caching_sha2_password` authenticates on a freshly
  started server without TLS. Before, the first login after a restart failed with ERROR 2061
  ("Authentication requires secure connection") until something else (phpMyAdmin) had primed
  the server's cache: the cause of the "not answering SQL after 45s" start-up message and of
  the migration import failures.
- Sidebar: the lock and theme buttons sit together again at the bottom left.

## [1.0.6] - 2026-11-11

### Fixed
- Migrate MariaDB → MySQL: MariaDB's default collations (`utf8mb4_uca1400_*`, unknown to MySQL)
  are rewritten to their MySQL counterparts in `COLLATE` clauses, so databases created on
  MariaDB 11.5+/13 import instead of failing at `CREATE DATABASE`. When the import client
  exits, its own message is reported (previously an empty "dump failed:").

## [1.0.5] - 2026-11-11

### Added
- Sites → Migrate: progress with a percentage (weighted by database size), the database being
  copied and the bytes streamed; the migration runs in the background, so the dialog can be
  closed and the Migrate button shows the progress until it is done. Every result is also
  appended to `logs/migrate.log` (readable from the Logs page).

### Fixed
- Migrate MariaDB → MySQL: the `/*!999999\- enable the sandbox mode */` line that new
  `mariadb-dump` versions emit is dropped from the stream; MySQL's client stopped on it with
  "Unknown command '\-'" before the first table, and the dump then hung on an unread pipe
  forever. When the import client dies, the dump is now stopped and the error reported.
- Migrate: the dump and import tools' stderr is drained while copying, so a flood of warnings
  can no longer fill the pipe and freeze a copy; errors now quote the last lines of that output.

## [1.0.4] - 2026-11-11

One release for everything since 1.0.1 (1.0.2 and 1.0.3 were built but never published).

### Added
- Sites → Migrate: copy the databases of Veranda's other engine (MariaDB ↔ MySQL) into the
  running one; the stored engine is started temporarily on a free port and stopped afterwards.
- **App lock** (Settings > Security): a PIN (digits) or a password asked for at launch and after
  an idle time you choose; a lock button in the sidebar; optional two-factor with a 6-digit code
  from an authenticator app (offline TOTP, setup key shown once). Only a salted PBKDF2-SHA256 hash is stored in
  `etc/secrets/app-lock.json`; while locked the backend refuses state commands. See
  `docs/SECURITY.md` for what it does and does not protect.
- **Looks** (Settings > Appearance): Porch (default), Glass (translucent, soft depth), Paper (flat)
  and Charcoal (neutral grey, brass accent), each in light and dark; the theme mode (system/light/
  dark) is also in Settings now.
- Terminal: a server stats strip (CPU, RAM, disk, uptime) under the bar of SSH terminals, read
  from `/proc` over the existing connection every 10 s while the terminal is visible. Disk is
  the fullest local filesystem; the tooltip lists every mount and the all-disks total.

### Fixed
- Database start no longer fails with "not answering SQL after 45s" while the server is up:
  an "Access denied" reply now counts as the server answering, and the message carries the
  client's own error text so the real cause is visible.
- Windows + Apache: the stop now uses `httpd -k shutdown` (the console-instance signal);
  `-k stop` addressed a Windows service that does not exist.
- Windows: **Start all** no longer flashes console windows - every helper program Veranda runs
  (clients, `initdb`, `netstat`, `icacls`, git, winget) is started without a console.
- Windows + Apache: every PHP page answered *Internal Server Error* (`AH01144: No protocol
  handler was valid ... (scheme 'balancer')`). Apache appends the file name to the `SetHandler`
  URL and Windows names start with a drive letter, so the balancer name became `verandaphpC:`.
  The handler URL now ends with a slash on Windows, and `SCRIPT_FILENAME` is rewritten to the
  plain path that php-cgi.exe expects.
- Windows + Apache: after a forced stop, the next start failed with `balancer slotmem_create
  failed` / `Configuration Failed`. Apache is now asked to stop through `httpd -k stop`, runtime
  files live in `run/` (`DefaultRuntimeDir`), and stale `.shm` files are removed before a start.
- Overview: the per-row enable switch is gone; the switch in the sidebar is the one place to
  hide or show a service. The row keeps its start/stop button.
- Windows: php-cgi workers each get their own OPcache (`opcache.cache_id`), a file cache fallback
  and no JIT, so a worker cannot die with "Unable to reattach to base address" and leave Apache
  answering *Internal Server Error* for PHP pages such as phpMyAdmin.
- Windows: URLs and folders are opened without `cmd /C start`, so a `&`, `^` or `%` in a URL or
  folder name can no longer be interpreted as a command.
- Glass look: scrolling is smooth again. The wash is one fixed composited layer instead of a
  fixed body background, and blur is limited to panels that float over content (sidebar,
  dialogs, toasts, snippets drawer, lock screen).
- Settings: hint text no longer touches the controls above it; the dark variants of the Glass
  and Paper looks are now clearly different from Porch.
- Terminal: at small window sizes the snippets panel floats over the terminal instead of
  squeezing it, and the bar keeps only what fits.

## [1.0.1] - 2026-11-11

### Fixed
- MySQL 8.3+/9.x on Windows failed to start with `unknown variable 'innodb_log_file_size'`: the generated my.ini now sets that variable for MariaDB only (MySQL uses `innodb_redo_log_capacity`).
- Database start-up waits until the server actually answers SQL instead of only checking the port; when the server exits during start-up the error now shows the last lines of its log instead of a bare "can't connect".

### Added
- Sidebar search: type to filter menus and services (for example `node`, `apache`, `mysql`); `Ctrl+K` or `/` focuses the box, `Enter` opens the first match, `Esc` clears.

## [1.0.0] - 2026-11-10

First stable release. The feature set of the 0.x series is considered complete; from here on
breaking changes bump the major version.

### Added
- Files: a search box above the tree filters by file name (case-insensitive, matches highlighted).
  Subfolders are listed on demand, skipping node_modules/vendor/target, at most 400 folders per search.
- Hosts & History: privacy mode (eye button in the page header, on by default, remembered) masks
  user names, addresses and ports as "•••". Each row has an eye to reveal its address for 15 seconds
  and a copy button that copies the real address without showing it.

### Changed
- Terminal: the tab tooltip and the bar under the tabs show the host label only; the address and
  port are no longer displayed. Quick connections are titled "Quick connection".

### Fixed
- Files: the "Hidden" checkbox was misaligned and its meaning unclear; it is now "Show hidden"
  with a hint, and lists dotfiles when ticked (unticked hides them, which is the default).

## [0.10.0] - 2026-11-09

### Added
- Linux .deb and .rpm installs can now **update in place**: the signed package from the
  release is downloaded, its signature verified with the updater key, installed through
  polkit (`pkexec`) and the app restarts. Before, these installs only showed "use the release
  page".

## [0.9.1] - 2026-11-09

### Fixed
- Release build: the Linux AppImage step failed in CI ("failed to run linuxdeploy"); the
  bundler now skips its own strip pass (the binary is already stripped) and the runner gets
  libfuse2.

## [0.9.0] - 2026-11-09

### Removed
- Trimmed to the essentials, at the maintainer's request: Caddy (Nginx is now the default web
  server; old configs saying `caddy` fall back to it), Valkey/Redis, Mailpit, Meilisearch,
  Qdrant, Composer (project templates use the `composer` on your PATH), CA bundle (PHP uses the
  system certificate store), Go, Bun, Java, Deno, Zig, .NET SDK and Cloudflare Tunnel (the
  Share buttons). Packages already installed stay on disk and in the registry; Veranda just
  no longer manages them. Startup groups drop services that no longer exist.

## [0.8.0] - 2026-11-09

### Added
- Settings > **Modules**: Remote (SSH), Files and Move to another computer can be switched off;
  they vanish from the sidebar and pages, nothing is uninstalled. The sidebar now groups
  **Local** and **Remote**.

### Changed
- Veranda describes itself as a local web workbench (dev stack, SSH client, file editor) in the
  README, installer description and CLI help.

## [0.7.0] - 2026-11-09

### Added
- **Files**: a built-in text editor (sidebar, and a pencil on each Sites row). Folder tree,
  syntax colouring for PHP, JS/TS, Python, Go, Rust, Java, C-like, Ruby, SQL, shell, CSS,
  HTML/XML, YAML, TOML, INI, .env, Dockerfile, Makefile; Markdown edit/split/preview, CSV as a
  table, JSON check and Format; Ctrl+S saves keeping line endings and permissions, with a
  conflict check when the file changed on disk; new file/folder, rename, delete.

### Fixed
- Windows: service logs over 20 MB were not trimmed (the file was still open while being
  replaced).

## [0.6.22] - 2026-11-09

### Added
- Settings > **Storage**: what the Veranda folder holds, and **Free space now** (also `veranda
  clean`): prunes debug builds, test suites, static libraries, man pages and headers from
  installed packages, gzips old SQL backups, deletes leftover downloads, trims logs over 20 MB.
- "Slim packages" (on by default): the same pruning happens right after a package is extracted.

### Changed
- Database backups are gzip-compressed (`.sql.gz`, 5-10x smaller); rotation, `veranda db
  import` and move bundles handle both forms. Old plain dumps are compressed on the next backup.
- MySQL runs with a 32 MB redo log instead of the 100 MB default; service logs are trimmed to
  their last 2 MB once they pass 20 MB.

### Changed
- Development: dev builds carry no dependency debuginfo, incremental compilation is off and
  `scripts/build-cool.sh` prunes `target/` above 20 GB (it had reached 76 GB).

## [0.6.21] - 2026-11-07

### Added
- Move bundles can carry a **Databases** section: a fresh dump of the user databases when the
  server runs (else the newest SQL backup). Restore loads it into the database, starting it if
  needed; `veranda db import <file>` does the same by hand.

### Changed
- Restore reports how many existing files were replaced (copies in `backups/move-<date>`), and
  the dialog explains that nothing is deleted.

### Fixed
- Scrolling inside a dialog no longer scrolls the page behind it.

## [0.6.20] - 2026-11-07

### Added
- Move to another computer: **Find projects** (and `veranda move suggest`) lists git, Composer
  and npm folders under the home and mounted drives; the chosen folder list is remembered in
  `veranda.toml` (`[move] folders`). After restore, Claude Code's per-project memory and
  `.claude.json` entries are re-linked to the folders' new paths.

## [0.6.19] - 2026-11-07

### Added
- **Move to another computer** (Settings, and `veranda move create|inspect|restore`): one
  password-protected `.veranda-move` bundle (PBKDF2 + AES-256-GCM, chunked and authenticated)
  with Veranda settings, sites, local CA, remote hosts/keys/snippets and their keyring
  passwords, projects, `~/.ssh`, git config and credentials, VS Code settings/keybindings/
  snippets, Claude Code settings/skills/agents/memory/login, and any extra folders. The bundle
  lists installed packages, git repositories, VS Code extensions and developer tools; restore
  puts files back (existing ones go to `backups/move-<date>`), reinstalls the packages, clones
  missing repositories, installs extensions and, on Windows, missing tools via winget.

## [0.6.18] - 2026-11-07

### Changed
- New look: a warm "porch" palette replaces the blue/purple one. Clay for actions, cream and
  timber surfaces, leaf green / honey / brick for states, in both light and dark mode. The app
  icon, tray icon and sidebar logo were recoloured to match (dark timber tile, terracotta roof).
  Host colour chips, the default terminal cursor/selection and the tray status dots follow suit.

## [0.6.17] - 2026-11-06

### Added
- Terminal: a **Snippets** panel (bar button or Ctrl+Shift+S) that stays open beside the
  session; search, click a name to type it in, "Press Enter after sending" toggle. It replaces
  the drop-down that closed after every pick.
- Snippets: optional **password lock**. The page and the terminal panel stay locked until the
  password is entered; snippet bodies are encrypted on disk (PBKDF2 + AES-256-GCM) and re-lock
  after an idle time you choose, on **Lock**, or when Veranda closes.
- Terminal themes: One Dark, Tokyo Night, Catppuccin Mocha, Gruvbox, Monokai; the default dark
  and light palettes were redrawn for contrast, with a deeper background and more padding.

### Changed
- Snippet lists show names only; the command text appears in the editor, never in a list.
- Snippets are typed into the terminal by the backend (`term_send_snippet`), so the UI never
  needs the text to send one.

## [0.6.16] - 2026-11-06

### Fixed
- Remote > Snippets: the Script box in "New snippet" collapsed to a few pixels wide (a shared
  dialog rule set stacked fields to zero width). Stacked fields now take the full dialog width.
- Headless check script: every dialog of the app can be opened by name
  (`scripts/ui-shot.sh snippets 24 remote:snippetdlg`, `projectdlg`, `migratedlg`, `svcgroupdlg`...)
  and each capture gets its own file name. All 16 dialogs were reviewed at 1360 px and 620 px.

## [0.6.15] - 2026-11-06

### Changed
- Update check: hourly instead of every 6 hours, and again when the window comes back to the
  foreground after 30 minutes or more (from the tray, after sleep). A release published while
  Veranda was already running no longer waits for "Check for updates".

## [0.6.14] - 2026-11-06

### Added
- Database page: **No password** removes root's password after a confirmation, for projects
  written for Laragon or XAMPP that connect as `root` with an empty password. The server keeps
  listening on 127.0.0.1 only. The CLI accepts `db set-password ""` for the same.

### Fixed
- Pages fill the window again on wide or maximised screens; the 1180 px column cap from 0.6.12
  left the right half empty.
- Tray menu: each service line now carries a green (running) or red (stopped) dot drawn as an
  icon, so it keeps its colour instead of inheriting the grey of the menu text. Clicking a line
  opens the window.
- Reinstalling the version a service is running (for example PHP 8.5.8 after the move to the
  static-php bulk bundle) restarts that service, so the new binary is what serves. Before, the old
  PHP-FPM kept running and phpMyAdmin still reported "mysqli is missing" until a manual restart.

## [0.6.13] - 2026-11-06

### Fixed
- "Install updates automatically" only acted 8 seconds after launch. It now also runs the moment
  the option is switched on, after "Check for updates", and every 6 hours while Veranda runs.
- When the signed update feed cannot be used, the banner says why (the updater's own message)
  instead of only "this install cannot update itself".

## [0.6.12] - 2026-11-06

### Fixed
- Package downloads could fail with "error decoding response body … stream error" or "peer
  closed connection": some download servers (static-php.dev's CDN among them) drop the
  connection part-way through large files. Downloads now use HTTP/1.1 and **resume from the last
  byte** with Range requests, retrying as long as progress is made, so a PHP install completes
  even on a flaky link. Version lists get the same retry.
- Settings written by a newer Veranda no longer stop an older build from starting: unknown keys
  in `veranda.toml` are ignored instead of rejected.
- "Trust local CA" installs the system-store copy with readable permissions (0644), which also
  silences a "Permission denied" warning from the TLS verifier.
- Wide windows: page content is capped at 1180 px and the version column of package tables no
  longer stretches, so columns stay next to each other on large screens.
- `scripts/ui-shot.sh` accepts `SCREEN=WxH` for wide-layout checks.

## [0.6.11] - 2026-11-06

### Added
- **Startup groups** (as in FlyEnv): named sets of services started in order and stopped in
  reverse from chips on Overview; one group can be the default started on launch.
- **Share a site**: a Cloudflare quick tunnel (`cloudflared`, installable under Tools) gives a
  public `https://….trycloudflare.com` address for the web root or any `*.test` site, after a
  confirmation; stop it from the address pill. No account needed.
- Cloudflare Tunnel package kind (GitHub releases with API digests, all platforms).

### Changed
- Security: on Linux/macOS the app disables core dumps for itself and every helper it starts and
  marks itself non-dumpable, so system crash reporters cannot upload memory images with secrets.
  `docs/SECURITY.md` gains a "where your data goes" review.
- Inputs, selects and text areas use 6 px corners instead of 8 px; search boxes match.
- Version lists: transient network errors (HTTP/2 stream resets, timeouts, 5xx) are retried
  twice, the retries over HTTP/1.1, instead of surfacing "error decoding response body".

## [0.6.10] - 2026-11-06

### Fixed
- PHP on Linux and macOS now comes from static-php.dev's **bulk** bundle instead of "common":
  it adds `mysqli` (phpMyAdmin refused to start without it), `intl` (Laravel, Symfony), `imagick`,
  `apcu`, `sodium`, `xsl`, `imap`, `swoole` and more. Downloads grow from about 20 MB to about
  31 MB per version. PHP versions installed earlier keep the smaller bundle: remove and install
  the version again in the PHP page to get the new extensions.

## [0.6.9] - 2026-11-06

### Fixed
- Remote: the password / passphrase / one-time-code prompt shows the host in its title and the
  full question as a subtitle, with a full-width answer box, instead of squeezing
  "Password for user@host" into a narrow label. The terminal bar keeps its buttons on one line:
  the target text shrinks with an ellipsis and the snippet picker narrows first.

## [0.6.8] - 2026-11-06

### Changed
- "Open" buttons for localhost tools (web root, Adminer, phpMyAdmin) use plain `http://localhost:<port>`:
  it is this computer, and https would first need the local CA to be trusted. Sites (`*.test`)
  keep HTTPS when it is enabled in Settings.

### Fixed
- Trust local CA on Linux: an older entry with Veranda's name in the browser store (for example
  the CA from before the rename) is replaced instead of making `certutil` fail with "exit status
  255"; the database is created when missing; every Firefox profile gets the CA too; and the tool's
  own message is shown when a step fails.

### Added
- Optional code signing in the release workflow: add the Apple (`APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`,
  `APPLE_TEAM_ID`) or Windows (`WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`) secrets and
  the installers are signed (and notarized on macOS). See `docs/SIGNING.md` for what to buy and
  why unsigned builds trigger SmartScreen and Gatekeeper warnings.

## [0.6.7] - 2026-11-06

### Fixed
- The local certificate authority created by versions before the rename was still called
  "Padu Local Development CA"; it is replaced once by a "Veranda Local Development CA" (the site
  certificate is reissued, and the CA must be trusted again if it had been).
- "Trust local CA" on Linux explains that `certutil` (package `libnss3-tools` / `nss-tools`) is
  needed for Chrome, Brave and Firefox, and where the CA file is for a manual import.

## [0.6.6] - 2026-11-06

### Fixed
- Release workflow: the draft release is created once before the platform builds start. The
  builds used to race for it, which produced two releases for v0.6.5 with the installers split
  between them (the published one had no Linux packages). This release carries every installer.

## [0.6.5] - 2026-11-05

### Changed
- The sidebar, Overview rows and service headers show each runtime's own logo (Caddy, Nginx,
  Apache, PHP, Python, Go, Bun, OpenJDK, Deno, Zig, .NET, MariaDB, MySQL, PostgreSQL, MongoDB,
  Meilisearch, Qdrant, Redis on Windows, Composer, Node.js, Adminer, phpMyAdmin) from Simple
  Icons (CC0), in the current text colour so both themes stay consistent. Valkey, Mailpit and
  the CA bundle keep Veranda's own icons.

## [0.6.4] - 2026-11-05

### Added
- Node.js: a **Use as default** button on every installed version (Veranda's own and nvm's).
  The chosen version goes first on the PATH of Veranda's terminals and project commands; for an
  nvm version it also becomes nvm's `default` alias, so your own shells follow. Stored as
  `[node] default` in `veranda.toml`.

### Fixed
- Services still bound to an older port setting are restarted automatically: once after the
  app starts (for example after an update) and whenever Settings are saved. The result shows as
  a normal start notification.
- Migrate report: the group headings ("Databases imported", "Errors") stay visible while the
  list scrolls.

## [0.6.3] - 2026-11-05

### Added
- The tray icon menu lists every service that is on, with its ports and whether it is running,
  and the tooltip says how many are running. The list refreshes after every start/stop and every
  20 seconds.

### Fixed
- A service that kept running with an older port setting (for example Apache still on 80/443
  after the ports were moved to 8080/8443) showed "Running" on the new ports. Overview now says
  "not listening on port …; restart to apply", and Start restarts it so the new ports apply.
- Migrate: entering Veranda's own database port as the source is refused with an explanation,
  instead of failing with "access denied".
- Version tables: the download progress text is no longer clipped; wide dialogs give two-column
  labels enough room so "Jump host" and "Startup snippet" stay on one line.

## [0.6.2] - 2026-11-05

### Fixed
- Migrate: FlyEnv is now detected from its data folder (`~/.config/FlyEnv`, `~/Library/FlyEnv`,
  `C:\FlyEnv-Data`) even when it has no `host.json` yet; the dialog then says that FlyEnv is
  installed but has no sites, instead of "not found". The FlyEnv folder itself can be given as the
  source, and sites are also read from its Apache/Nginx vhost files. The folder picker always
  picks folders, and the dialog preselects an installed stack.
- Migrate: Laragon and XAMPP are looked for on every drive letter (C: to Z:), in Program Files,
  and in the user's home folder, not only on C: and D:.

## [0.6.1] - 2026-11-05

### Fixed
- Dialogs (New host, rules, keys, settings) no longer overflow small windows: columns shrink,
  labels stack below 560 px, and tall dialogs scroll inside the window.
- Narrow windows: host, key and snippet actions wrap to a second line, the terminal bar wraps,
  the quick-connect field shrinks, the SFTP browser stacks its panes, and the collapsed sidebar
  hides counters.
- Links clicked inside a terminal open in the browser (any http/https URL, not only GitHub).
- Starting a tunnel or SOCKS proxy bound to anything but loopback now asks for confirmation.
- SFTP listings ignore entry names containing path separators (a hostile server could otherwise
  steer a download outside the chosen folder).
- Credential-store calls (KWallet, GNOME Keyring...) run with a 45 s limit and fall back to the
  private secrets file instead of freezing the app when the consent dialog never answers.
- Dependencies refreshed (`cargo update`), including a yanked `async-executor` release.

### Added
- `docs/USAGE.md` and `docs/USAGE.id.md`: a walkthrough of every screen, linked from the READMEs.
- CI accepts RUSTSEC-2023-0071 (`rsa` Marvin timing side channel, no fixed release) with the
  reasoning recorded in `.cargo/audit.toml` and `docs/SECURITY.md`.

## [0.6.0] - 2026-11-04

### Added
- **Remote**: a built-in SSH client in the sidebar, modelled on Termius, so you can manage your
  servers from the same window as your local stack.
  - **Hosts**: label, address, port, user, groups (nested, with an inherited identity), tags,
    colours, notes, jump host (ProxyJump), startup snippet, keep-alive, agent forwarding,
    environment variables; search and tag filters; Quick connect (`user@host:port`); the server's
    OS is detected after the first login. Import from `~/.ssh/config` (and PuTTY sessions on
    Windows), including the keys named in `IdentityFile`.
  - **Terminal**: tabs with SSH and local shells (xterm.js), 256 colours and true colour,
    search, copy-on-select, right-click paste, clickable links, themes (follow Veranda, dark,
    light, Dracula, Solarized, Nord), font and cursor settings, reconnect, snippet sender. The
    local shell has Veranda's runtimes (PHP, Node, Composer...) on its `PATH`.
  - **SFTP**: dual-pane browser (this computer / server) with upload and download of files and
    folders, drag and drop between the panes, transfer queue with progress and cancel, new
    folder, rename, delete, chmod, hidden files, and an in-app editor for text files.
  - **Port forwarding**: local, remote and dynamic (SOCKS5) rules per host with a start/stop
    switch and live connection counters.
  - **Snippets**: saved commands grouped in folders; send to the active terminal or run on
    several hosts at once and compare the output.
  - **Keychain**: generate Ed25519/RSA/ECDSA keys (optionally passphrase-protected), import from
    file or paste, export the public or private key, change the passphrase, install the public key
    on a host (like `ssh-copy-id`), and identities (username + password/key) reusable across hosts
    and groups.
  - **Known hosts** (trust on first use with fingerprint confirmation; a changed key is flagged
    loudly) and a connection **History**.
  - Authentication: key, password, SSH agent (`SSH_AUTH_SOCK`, Windows OpenSSH agent, Pageant),
    keyboard-interactive (one-time codes). Passwords and passphrases are stored in the operating
    system's credential store (Keychain, Credential Manager, Secret Service/KWallet); without one
    they go to a private file under `secrets/`. Private keys live in `etc/remote/keys/` with
    user-only permissions. The SSH stack is pure Rust (russh + ring); no OpenSSH binary needed.

## [0.5.0] - 2026-11-04

### Fixed
- Logs view strips ANSI colour codes (Meilisearch and others print them).
- Large downloads on slow connections could fail with "operation timed out" after 30 minutes even
  though they were still progressing; the deadline is now a 2-minute stall timeout instead.

### Changed
- The project moved to https://github.com/livrensandi1/veranda; updates are checked there (the old
  address keeps redirecting, so installed copies continue to update).

### Added
- A new hand-drawn icon family (24-grid rounded line icons) for every menu, service, action and
  toast, and a vector app icon drawn to match: dark and light tiles, used for the launcher,
  installers, tray and in-app brand.
- Services: **MongoDB** (official archives), **Meilisearch** and **Qdrant** (GitHub releases), each
  with its own data folder, loopback-only listener and port setting; Meilisearch and Qdrant have an
  "Open UI" button. MinIO was considered and dropped: the open-source project was archived upstream.
- Runtimes: **Java** (Eclipse Temurin LTS lines via Adoptium), **Deno**, **Zig** and the **.NET SDK**,
  all with upstream checksums and added to the terminal PATH.

### Changed
- Fresh installs start with every service switched off: the Overview stays empty until you turn a
  service on in the sidebar, and what you turned on stays there across restarts until you turn it
  off. (Existing installs keep their current switches.)

## [0.4.4] - 2026-11-04

### Fixed
- Linux: WebKitGTK's GPU rasteriser crashed its render process on some Intel/Mesa drivers
  ("WebKitWebProcess has encountered a fatal error"). Veranda now rasterises on the CPU and keeps
  GPU compositing (same 60 fps in the scroll benchmark). If the render process still dies, the
  page reloads itself within 30 seconds instead of staying blank.
- Linux: every quit produced a "WebKitWebProcess has encountered a fatal error" notification. The
  core dump shows a use-after-free in libEGL's exit handlers inside WebKit's helper process (a
  Mesa bug, not reachable from the app). Veranda now ends its WebKit helper processes directly at
  quit, so no exit handler runs and nothing is reported.

### Changed
- Turning a service switch on no longer silently enables "Start services on launch"; that option
  is only set in Settings. Services started at launch now report their outcome (toast, port
  dialogs) exactly like a manual Start All, so a failed start is no longer invisible.
- In-app brand uses the dark and light tiles from the owner's icon showcase sheet (the installer
  icon keeps the higher-resolution source).

## [0.4.3] - 2026-11-04

### Added
- A cancel button on a running download: the partial file is deleted and nothing is installed.
- Node.js: nvm-managed versions get a "Remove via nvm" button (runs `nvm uninstall`).

## [0.4.2] - 2026-11-04

### Added
- Node.js: when nvm (or nvm-windows) is installed, Install runs `nvm install <version>` so the
  version lands in nvm; without nvm it is installed into Veranda's folder as before.

### Changed
- In-app brand icon follows the theme: pale glass tile in light mode, dark glass tile in dark mode.
- Overview lists only services that are switched on (or still running); switched-off ones live in
  the sidebar until you turn them on.
- The cache entry is named Valkey on Linux/macOS and Redis on Windows, with a note explaining that
  Valkey is the open-source continuation of Redis 7.2 (same commands, port and clients).

### Fixed
- CI on Windows: the new registry test also wrote a Windows path into hand-made JSON.
- A registry file that is not valid JSON is backed up as `etc/installed.broken-<time>.json`
  before the registry is rebuilt from the package folders.

## [0.4.1] - 2026-11-04

### Fixed
- CI on Windows: the FlyEnv import test wrote a Windows path into hand-made JSON (invalid escape).
- Installed packages could "disappear" from the app when `etc/installed.json` held an entry an
  older build could not read (for example after running a newer build on the same root): the
  whole file was discarded and the next install overwrote it. The registry now keeps entries it
  cannot read, repairs moved paths, and re-registers every package folder under `bin/` that the
  file does not mention, so packages on disk are never shown as uninstalled.

### Changed
- Version lists show a clear loading state while upstream is queried: a progress bar on the table,
  shimmering placeholder rows and a status line; new rows fade in when the list arrives.

## [0.4.0] - 2026-11-04

### Added
- **PostgreSQL** as a second database service (runs beside MariaDB/MySQL): official binaries via
  the zonky.io Maven packages with SHA-256 checks, data in `data/postgres`, superuser `postgres`
  with a generated password, loopback only, Adminer link with the PostgreSQL driver preselected.
- **Python** (python-build-standalone), **Go** (go.dev) and **Bun** (GitHub) as downloadable
  runtimes with upstream checksums; they are added to the terminal PATH.

### Fixed
- Linux: after granting port 80/443 permission, the web server still refused to start because
  Veranda probed the port from its own (unprivileged) process. The probe now trusts a binary that
  carries `cap_net_bind_service`, so "Allow" works in one go.

### Added
- When another program holds a port a service needs, Veranda names the program and offers to
  move the service to the next free port (80 -> 8080, 443 -> 8443, others +1) in one click.

### Changed
- New app icon: a glass tile with a house roof over a server stack (artwork supplied by the
  project owner), used for the window, tray, installers and the in-app brand.
- Sidebar and dashboard switches now start or stop the service immediately (as in FlyEnv) and
  mark it to come up when Veranda launches. Hint texts updated.
- Node.js: versions installed by nvm, nvm-windows, fnm or volta are detected and listed on the
  Node.js page (with their folder) instead of being offered for download again.

## [0.3.0] - 2026-11-04

### Added
- **Migrate** (Sites page): import projects from Laragon, XAMPP, FlyEnv or any folder, linked in
  place or copied into the document root, and optionally copy databases from the source server
  through Veranda's own `mysqldump`/client (the source stack must be running).

### Changed
- Scrolling is handled by the window again instead of an inner pane, which the compositor
  accelerates; the sidebar is sticky. Measured with the built-in benchmark (`VERANDA_UI_PERF=1`):
  60 fps with GPU rendering on, 19 fps with it off, so GPU rendering stays the default.
- Version tables use fixed status/action columns so rows line up; service-page actions align right.

## [0.2.0] - 2026-11-03

### Changed
- **Renamed to Veranda** (formerly Padu). Binaries, package names, the app identifier
  (`dev.veranda.desktop`), the config file (`etc/veranda.toml`), the `VERANDA_HOME` variable and
  the default root (`~/veranda`) all follow the new name. An existing `~/padu` root is moved to
  `~/veranda` automatically on first start when no service is running, and the old managed block
  in the hosts file is replaced. Uninstall the old Padu package after installing Veranda.
- New icon and a service-centred layout: every service and tool has its own page with a
  Service tab (status, ports, actions, files) and a Versions tab (install / remove any version),
  and the sidebar switches choose which services the dashboard starts.
- Linux: GPU rendering is on by default (was forced off); `general.hardware_acceleration = false`
  turns it off for drivers that flicker. The window appears only after the first render, with a
  dark background, so there is no white flash on launch.

## [0.1.3] - 2026-11-03

### Fixed
- Linux: a privileged port (80/443) that the OS refused to bind was reported as "already in use".
  It is now detected separately and the desktop app offers the one-time permission
  (`setcap`, asks for your password) or a switch to 8080/8443.
- MySQL/MariaDB on Debian/Ubuntu 24.04+: the generic tarball needs `libaio.so.1`, which the distro
  ships as `libaio.so.1t64`. Veranda now links it into the package's private lib folder; if libaio is
  missing entirely, the error says which package to install.

### Changed
- Dashboard: every service shows a clear Running / Stopped / Starting… / Stopping… badge, running
  rows are highlighted, buttons show progress while a request runs, and results read
  "PHP stopped" / "All services stopped" instead of "OK". Stop errors have their own message.

## [0.1.2] - 2026-11-03

### Changed
- Updates always come from the official repository (`forallgame/veranda`); the GitHub repo field was
  removed from Settings (forks can still set `update.repo` in `veranda.toml`).

### Fixed
- Dashboard: the on/off switch of a service whose package is not installed is now shown off and
  locked, so it no longer looks as if the package were installed and enabled.

## [0.1.1] - 2026-11-03

### Fixed
- Smoother scrolling: the UI only re-renders when data actually changed, and the Linux build
  disables WebKitGTK's DMA-BUF renderer, which caused stuttering on many GPUs.
- Launching Veranda while it is already running (for example from the tray) now focuses the existing
  window instead of starting a second process with a second tray icon.
- README download commands use the real asset names.

### Added
- Dashboard customization (slider button): choose the web server, the database engine and which
  services are shown. Hidden services are not started by Start All.
- Close behaviour setting: ask every time (default, with "remember my choice"), hide to tray, or
  quit. Changeable later in Settings.

### Changed
- Redesigned buttons, switches, tiles and navigation highlight.

## [0.1.0] - 2026-10-31

### Added
- Core engine (`veranda-core`): configuration, package manifest with verified versions, HTTPS-only
  downloader with SHA-256/SHA-512/MD5/remote checksum verification and a trust-on-first-use ledger,
  zip/tar.gz/tar.xz extraction with path-traversal protection, process supervision with pid files.
- Services: Caddy (default), Nginx and Apache front-ends; PHP-FPM (Linux/macOS) and php-cgi workers
  (Windows); MariaDB and MySQL with automatic initialisation and random root password; Valkey/Redis;
  Mailpit.
- Automatic virtual hosts (`<folder>.test`) with `public/` detection, manual sites, hosts-file
  management through a validating elevated helper.
- Local certificate authority (ECDSA P-256) and site certificates; trust-store import commands for
  the current user on all three platforms.
- Run-time version discovery for PHP (php.net / static-php), Caddy, Mailpit, MariaDB, MySQL,
  Node.js, Composer, Adminer, phpMyAdmin, Nginx, Apache, Valkey/Redis and the Mozilla CA bundle.
- Project creation: blank, WordPress, Laravel, Symfony, any Composer template, git clone.
- Database helpers: create/drop/list, password rotation, full dumps with rotation.
- `veranda` CLI covering every feature, plus `doctor` and self-update check against GitHub releases.
- Tauri 2 desktop app with dashboard, packages, sites, database and settings pages, tray icon,
  light/dark/system theme, English and Indonesian UI, background backups.
- Desktop: Logs tab (per-service log tail with error/warning highlighting, clear, open folder) and an
  in-app activity history; packages grouped by category with a version picker (built-in or newest
  upstream); per-version uninstall with in-app confirmation (blocked while the service runs);
  inline download/extract progress per package; compact animated notifications.
- In-app auto-update (Tauri signed updater; GitHub Releases `latest.json`; Windows, macOS and
  Linux AppImage), with GitHub-API fallback for installs that cannot self-update.
- Released under the MIT License; README in English and Indonesian with per-OS install guides.
- Legal hygiene: clean-room implementation, permissive-only dependency audit, `THIRD_PARTY_NOTICES.md`
  generated from `cargo metadata`, runtime licenses documented, no bundled third-party assets or fonts.
- CI workflow (fmt, clippy, tests on Linux/Windows/macOS) and release workflow (installers on tag).
