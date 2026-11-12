# Using Veranda

A walkthrough of every screen. Bahasa Indonesia: [USAGE.id.md](USAGE.id.md).

Veranda has two halves. The **local stack** (Overview, Sites, Database, Logs and the service
pages) runs web servers, languages and databases on your own computer. The **Remote** group is an
SSH client for the servers you manage elsewhere. Nothing in either half leaves your computer
unless you connect to a server you chose; there are no accounts and no telemetry.

## 1. First start

1. Install Veranda ([README](../README.md#install)) and open it. The window starts on
   **Overview**, which is empty on a fresh install.
2. In the sidebar, click a service you want (for example **PHP**, **Nginx**, **MariaDB**). Each
   service page has two tabs: **Service** (state, ports, buttons) and **Versions** (what you can
   download). Pick a version and click **Install**. Downloads are verified against the vendor's
   checksums.
3. Turn the switch next to the service **on**. It starts right away and appears on Overview.
4. Open <http://localhost/> (or **Start All** on Overview to start everything that is on).

The local stack stores everything under one folder, the **Veranda root** (`~/veranda` by
default; shown in Settings). Deleting that folder removes all runtimes, sites and data.

## 2. Overview

**Startup groups** (the chips above the service list) start and stop a named set of services in
one click, in the order you chose: for example "Laravel stack" = Nginx, PHP, MariaDB. The
group marked as default starts when Veranda launches, if "Start services on launch" is on.


- The status pill shows whether everything that is on is running.
- **Start All / Stop All** act on every service whose switch is on.
- Each row shows the version in use (a drop-down when several are installed), its ports and the
  start/stop/restart buttons. The switch turns a service off: it is stopped and hidden from
  Overview until you turn it on again in the sidebar.
- The tiles open the web root, Adminer, the mail viewer, a terminal in your document root, the
  `www` folder, and Logs.

If a port is already taken, Veranda names the program using it and offers the next free port.
On Linux, ports below 1024 need a one-time permission; Veranda asks for your password once and
grants it to the web server binary only (or switch to 8080/8443 in Settings).

## 3. Sites


Every folder inside `www/` becomes `https://folder-name.test` automatically. Laravel and Symfony
style `public/` folders are detected. For a project that lives elsewhere, use **Add external
folder** (link in place) or **Migrate** to bring projects and databases over from Laragon, XAMPP
or FlyEnv. Migrate looks for each stack in its usual place; for FlyEnv it reads the host list
(`host.json`) or the Apache/Nginx vhost files under its data folder, and tells you when FlyEnv is
installed but has no sites yet (then use "Other folder of projects" and point at the folder that
holds your projects). **New project** creates a folder from a template (plain, Composer package via your system `composer`, Git clone)
and, optionally, a database. **Sync** rewrites the virtual hosts and your hosts file (asks for
elevation once).

## 4. Database

Host, user and password of the local MariaDB/MySQL server, a one-click **New password**, create
databases, open Adminer or phpMyAdmin, and **Backup** (dumps every database into `backups/`;
automatic backups are configured in Settings). PostgreSQL has the same card on its own page.

Veranda gives `root` a random password on first start. A project written for Laragon or XAMPP
usually expects `root` with no password: either paste the password from this card into the
project's `.env`, or click **No password**. The server only listens on `127.0.0.1`, so a
password-less root is still reachable only from your own computer.

### Switching between MariaDB and MySQL
Each engine keeps its own data folder (`data/mariadb`, `data/mysql`), so switching the engine in
the sidebar starts with the other folder and your databases seem to be gone. They are not:
**Sites → Migrate** lists *Veranda MariaDB → MySQL (databases stored here)* (or the reverse).
Pick it and press **Migrate now**: Veranda starts the stored engine on a free port, copies every
database into the running engine with that engine's own dump tool, then stops it again. Nothing
in the old data folder is changed, so you can switch back at any time. The dialog shows a
percentage (weighted by database size) with the database being copied and the bytes streamed;
the window can be closed meanwhile, the **Migrate** button then shows the progress and opens the
dialog again with the result. **Pause** holds the copy (nothing is lost on resume);
**Cancel after this database** finishes the database being copied and then stops, and a second
press, **Stop now**, ends the current copy, which is simply copied again next time. Every result is also appended to `logs/migrate.log`, so a migration that
finished while nobody was looking can still be checked on the Logs page. Databases with the same
name in the running engine are replaced. If the root password was changed after the switch, the
old engine still has the previous one; type it in the password field.

## 5. Logs

The **Activity** box lists what Veranda itself did (started, stopped, installed, errors). The
drop-down opens the log file of any service; errors and warnings are coloured.

## 5b. Files
A small editor for project files, in the sidebar under Logs (and the pencil on every row of
Sites). Pick a folder (the document root, your move-bundle folders, your home, or any other) and
a file. Markdown opens side by side with a rendered preview (headings, lists, task lists,
tables, code blocks, links; external links open in the browser, relative links open the file),
CSV as a table, JSON with a validity check and a **Format** button. Code is coloured for
PHP, JavaScript/TypeScript, Python, Go, Rust, Java, C-like, Ruby, SQL, shell, CSS, HTML/XML,
YAML, TOML, INI, .env, Dockerfile and Makefile. Ctrl+S saves (line endings are kept as they
were), the file's permissions are preserved, and a file that changed on disk in the meantime is
not overwritten without asking. Right-click a tree entry to rename or delete it. Files above
4 MB and binary files are not opened.

## 6. Settings

General (language, TLD, document root, editor, start on launch, start minimized, close button
behaviour, GPU rendering on Linux), web ports and HTTPS, PHP limits, database/cache/mail ports,
backups and updates. **Trust local CA** installs Veranda's certificate authority into your
browser's store so `https://*.test` shows no warning. **Config file** opens `veranda.toml` for
anything not in the form.

### Appearance
**Theme** follows the system or forces light/dark. **Look** picks the palette: Porch (warm
default), Glass (translucent surfaces over a soft wash), Paper (flat, ink on off-white) or
Charcoal (neutral grey with a brass accent). Both are saved on this computer and apply at once.

### Security: app lock
**Set PIN or password** asks for a PIN (digits) or a password (any text, 4 characters or more)
and from then on Veranda asks for it when it starts and after the idle time in **Lock after** (or only when you press the
lock button in the sidebar). Services keep running while locked; the window is blurred and the
backend refuses commands until the right secret is entered. After five wrong attempts you have to
wait, longer each time. Only a salted hash is stored (`etc/secrets/app-lock.json`); there is no
recovery, so if you forget it delete that file while logged in to this computer. The lock guards
Veranda itself; to protect your project files and databases on a stolen laptop, turn on disk
encryption (BitLocker on Windows, FileVault on macOS, LUKS on Linux) and a login password.

**Two-factor** (optional, once the lock is on): **Enable** shows a setup key for an authenticator
app (Aegis, Google Authenticator, Authy, 1Password, Bitwarden…); type the key into the app, then
the 6-digit code it shows. From then on the lock screen asks for PIN or password plus the current
code. Keep the key somewhere safe: without the app and the key the lock cannot be opened (the
last resort is deleting `etc/secrets/app-lock.json` while logged in). Everything runs offline;
no phone number, no server.

### Modules
Veranda's core is the local server stack. **Remote** (SSH), **Files** (editor) and **Move to
another computer** are modules you can switch off here; they disappear from the sidebar and the
pages until switched on again. Nothing is uninstalled or deleted. The rule for what belongs in
Veranda: it must touch your local projects or the servers they run on.

### Storage
The **Storage** card shows what the Veranda folder holds (packages, databases, backups, logs,
projects). **Free space now** removes debug builds, test suites, static libraries, man pages
and headers from installed packages (MySQL alone carries about 550 MB of those), gzips old
plain-text SQL backups, deletes leftover downloads and trims logs above 20 MB. Databases,
projects and settings are never touched. New packages are slimmed the same way as they are
installed ("Slim packages" in General). Backups are written gzip-compressed (`.sql.gz`);
`veranda db import` and move bundles read both forms. From the terminal: `veranda clean`.

### Move to another computer
**Create bundle** packs everything personal into one `.veranda-move` file protected by a
password (PBKDF2 + AES-256-GCM): Veranda settings, sites, the local CA, remote hosts, keys,
snippets and their passwords; the projects in the document root (without `node_modules`,
`vendor`, `target` and the like unless you tick "Keep"); `~/.ssh`; git config and credentials;
VS Code settings, keybindings and snippets; Claude Code settings, CLAUDE.md, skills, agents,
commands, plugins, project memory and login (never session transcripts or caches); and any
extra folders you add (**Find projects** lists every git, Composer or npm folder under your home
and mounted drives; the list you pick is remembered for the next bundle). The bundle also
records the installed Veranda packages, git repositories (remote and branch), VS Code
extensions and developer tools found on `PATH`. Per-project settings (`.vscode/`, `.claude/`,
`CLAUDE.md`, `.env`) travel inside their project folder, and Claude Code's per-project memory
is re-linked to the folder's new path after restore. **Databases** adds a fresh SQL dump of
your MariaDB/MySQL databases when the server is running (otherwise the newest backup Veranda
made); PostgreSQL is not included yet.

Restoring never deletes anything. A file that already exists on the new computer is moved to
`backups/move-<date>` before the copy from the bundle takes its place, and files the bundle does
not contain stay as they are. So restoring onto a computer that already has data merges the two:
same-named files come from the bundle, everything else is kept. Database dumps are loaded into
the running database (started if needed) by the finish step, or later with
`veranda db import <file>`; databases with the same name are overwritten by the dump.

A bundle is a snapshot, not a sync: make a fresh one right before you switch computers.

On the new computer, install Veranda, open Settings, **Restore a bundle**, pick the file and
the password, tick the parts you want and press Restore. Files that already exist are moved to
`backups/move-<date>` first. Afterwards Veranda reinstalls the same package versions (or the
closest available for that platform), clones repositories that are missing, installs the VS
Code extensions when `code` is on `PATH`, and on Windows can install missing tools with
`winget`. Programs themselves never travel: a Linux binary does not run on Windows.

The same works from the terminal: `veranda move create <file>` (add `--dry-run` to see sizes,
`--folder <path>` for extra folders, `--only ssh --only git` to limit; `veranda move suggest`
lists project folders), `veranda move inspect
<file>` and `veranda move restore <file>`. The password comes from `VERANDA_MOVE_PASSWORD` or a
prompt. There is no recovery for a forgotten password.

## 7. Remote (SSH client)

### Hosts
Click **New host**: label, address, port, username and how to log in (**Auth**):

| Auth | What it does |
| --- | --- |
| Password | Uses the password you type here (stored in the system credential store) or asks when connecting |
| Key | A key from the Keychain |
| Identity | A reusable username + password/key from the Keychain |
| SSH agent | Keys loaded in your agent (`ssh-agent`, Windows OpenSSH agent, Pageant) |
| Ask when connecting | No stored credential; the server's prompts are shown |

Optional: **Group** (hosts inherit the group's identity), **Jump host** (connect through another
host first), **Startup snippet** (sent after login), tags, a colour, environment variables sent
with the session, notes, keep-alive and agent forwarding.

Double-click a host to open a terminal. The row buttons open a terminal, SFTP, edit, duplicate
and delete. **Quick connect** (`user@host:22`) connects without saving. **Import** reads
`~/.ssh/config` (and PuTTY sessions on Windows) and adds the hosts, with their keys if you tick
"Import private keys".

The first time you connect to a server its key fingerprint is shown. Compare it with what the
server's administrator published, then **Trust**. A server whose key later changes is flagged in
red; only continue if you know why (reinstalled server). Accepted keys are listed under **Known
hosts**, where you can forget one.

### Terminal
Tabs hold SSH and local sessions (**+** or Ctrl+Shift+T opens a local shell with Veranda's PHP,
Node and friends on `PATH`). In the bar: state, **Snippets** (Ctrl+Shift+S opens a
panel beside the terminal: search, click a name to type it into the session, and the panel stays
open until you close it), search (Ctrl+Shift+F), clear, reconnect, settings and close.
Ctrl+Shift+C/V copy and paste; selecting text copies it when "Copy on select" is on; right-click
pastes. The gear sets font, size, theme (follow Veranda, dark, light, One Dark, Tokyo Night,
Catppuccin, Dracula, Gruvbox, Monokai, Solarized, Nord), cursor, scrollback, keep-alive interval,
the local shell program and whether closing a session asks first.

#### Server stats
Under the bar of an SSH terminal a thin strip shows **CPU**, **RAM** and **Disk** of the
server plus its uptime. Disk is the fullest local filesystem (the one that runs out first; its
mount is shown when it is not `/`); hover it for every mount and the all-disks total that cloud
consoles usually report, which is why their percentage can be lower. Values are read from `/proc` and `df` on the existing connection every 10 seconds
while that terminal is the visible one. It costs the server one tiny command and the app nothing
noticeable; nothing runs while the tab is hidden. Click the strip to hide it; the terminal
settings (gear button) have a checkbox to bring it back. Servers
without `/proc` (BSD, macOS) show what they have.

#### Small windows
When the terminal area is narrower than about 640 px the snippets panel slides over the
terminal instead of squeezing it; a click in the terminal closes it. The bar drops the host
name and button labels first, then the status text.

### SFTP
Pick a host and **Connect**. The left pane is this computer, the right pane the server.
Double-click folders to enter, use the path box, **↑**, home and refresh. Select files (Ctrl or
Shift for several) and press the arrow between the panes, or drag them across, to upload or
download; folders are transferred with their contents. The transfer list shows progress and lets
you cancel. Row buttons rename, delete, change permissions (server) and open a text file in a
small editor (server).

### Port forwarding
**New rule** with a type:

- **Local**: `127.0.0.1:3307 → 127.0.0.1:3306` through the server lets local tools reach the
  server's database on port 3307.
- **Remote**: the server listens on a port and sends connections to a service on your computer.
- **Dynamic**: a SOCKS5 proxy on your computer; point a browser at it to browse through the
  server.

The switch starts and stops a rule; the row shows how many connections are open. Keep the bind
address on `127.0.0.1` unless you really want other machines on your network to use the tunnel.

### Snippets
Commands you run often, grouped by folder. Lists show names only, never the command text; the
editor shows it. **Send** types one into the active terminal (the terminal's Snippets panel does
the same with one click); **Run on hosts** executes it on the hosts you tick and shows each
host's output and exit code side by side.

**Set password** locks the page and the terminal panel until the password is entered, and
encrypts every snippet body on disk (AES-256-GCM, key derived from the password with PBKDF2).
Unlocking lasts until Veranda closes, until you press **Lock**, or after the idle time you chose.
Startup snippets and "Run on hosts" also need the lock open. There is no recovery: a forgotten
password means the snippet bodies are gone, so keep the password somewhere safe.

### Keychain
**New key** generates an Ed25519 (recommended), RSA 4096 or ECDSA key, optionally protected by a
passphrase. **Import key** reads a file from `~/.ssh` or pasted text. Per key: copy the public
key, **Install on host** (appends it to the host's `authorized_keys`, like `ssh-copy-id`), export
the public or private file, change the passphrase, delete. **Identities** combine a username with
a password or key so several hosts and groups can share one login.

### History
Every connection with its time, duration and outcome; click the terminal button to connect again.

## 8. Updates

Windows, macOS and Linux AppImage installs update in place through the signed updater feed.
Linux **.deb and .rpm installs** update too: Veranda downloads the package from the release,
checks its signature with the same key, and installs it through the system's password prompt
(polkit/`pkexec`), then restarts. If `pkexec` is missing, the banner tells you where the
downloaded package is so you can install it with `sudo dpkg -i` or `sudo rpm -U`.

Veranda checks the GitHub releases of this project (github.com/livrens/veranda) 8 seconds after start, every hour, and when the window comes back to the foreground after 30 minutes or more, and
shows a banner; **Update now** downloads the signed installer and restarts. With **Install
updates automatically** on, the download and restart happen by themselves: right when you switch
it on, at each check, and after **Check for updates**. Self-update works for .deb, .rpm and
AppImage on Linux, the NSIS/MSI installers on Windows and the .app on macOS; the banner says so
when an install cannot update itself (then use the release page). Service versions
(PHP, MariaDB and the rest) are listed from the vendors' own servers in each Versions tab.

## 9. Where your data is

| Data | Location |
| --- | --- |
| Settings | `<root>/etc/veranda.toml` |
| Installed runtimes | `<root>/bin/<kind>/<version>` |
| Websites | `<root>/www` |
| Databases | `<root>/data` |
| Logs, backups | `<root>/logs`, `<root>/backups` |
| Local certificate authority | `<root>/etc/ssl` (private) |
| Database root password | `<root>/etc/secrets` (private) |
| Remote hosts, groups, snippets, rules, history | `<root>/etc/remote/store.json` (private; snippet bodies encrypted when a snippet password is set) |
| Snippet password check (salt + verifier, never the key) | `<root>/etc/remote/snippet-lock.json` (private) |
| SSH private keys | `<root>/etc/remote/keys` (private, 0600) |
| Accepted server keys | `<root>/etc/remote/known_hosts` |
| SSH passwords and passphrases | system credential store (Keychain, Credential Manager, Secret Service); fallback `<root>/etc/secrets/remote-secrets.json` |

Back up the root folder to keep everything; never share the `etc/secrets` or `etc/remote/keys`
folders.

## 10. Troubleshooting

- **phpMyAdmin says "the mysqli extension is missing"**: the PHP version was installed with an
  older, smaller bundle. Open the PHP page, remove that version and install it again.
- **A service will not start**: open Logs, pick the service's log. The last lines say why
  (usually a port in use, which Overview also reports with an offer to move).
- **`*.test` does not resolve**: Sites → **Sync** updates the hosts file (needs elevation once).
- **Browser warns about the certificate**: Settings → **Trust local CA**, then restart the
  browser. Linux needs `certutil` (`sudo apt install libnss3-tools` or `dnf install nss-tools`)
  for Chrome, Brave and Firefox. Firefox on Windows/macOS reads system certificates only when
  `security.enterprise_roots.enabled` is `true` in `about:config`, or import `etc/ssl/ca.crt`
  by hand. Localhost tools (Adminer, phpMyAdmin) open over plain http and never need this.
- **"Unknown publisher" / Gatekeeper warnings when installing**: the installers are not code-signed
  yet; see [SIGNING.md](SIGNING.md).
- **"Server key changed" when connecting over SSH**: the server was reinstalled, or something is
  intercepting the connection. Confirm with the administrator before trusting the new key.
- **Password prompt although a password is saved**: the system credential store refused or timed
  out; Veranda falls back to a private file and says so on the Keychain page.
- **Linux: black or flickering window**: Settings → untick **GPU rendering**, restart Veranda.
