# Building Veranda

Requirements on every platform: Rust stable (1.90+) via rustup. The desktop app additionally needs
the Tauri 2 prerequisites.

## CLI (all platforms)

```sh
cargo build --release -p veranda-cli
# binary: target/release/veranda (veranda.exe on Windows)
```

Run the test-suite: `cargo test -p veranda-core`.

## Desktop app

Install the Tauri CLI once: `cargo install tauri-cli --version "^2" --locked`.

### Linux (Debian/Ubuntu/Kubuntu)
```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev \
  libsoup-3.0-dev libjavascriptcoregtk-4.1-dev patchelf build-essential curl wget file libssl-dev
cd apps/desktop/src-tauri
cargo tauri dev          # development
cargo tauri build        # .deb, .rpm and .AppImage in target/release/bundle/
```
Tip for laptops that run hot: use `scripts/build-cool.sh` (one job, lowest priority, pauses the
compiler above 80°C and resumes below 65°C), e.g. `scripts/build-cool.sh build -p veranda-desktop`.
Headless UI check on Linux: `scripts/ui-shot.sh <tab> [seconds] [autotest]` runs the debug app on
Xvfb with a scratch root under `target/ui-root` and writes a screenshot to `target/ui-shots/`
(needs `xvfb`, `ffmpeg`, `dbus-run-session`); `SIZE=600x480 THEME=light` vary window and theme.
For release builds and installers prefer the GitHub Actions workflows: they run on GitHub's
machines, not on your laptop.

### Windows
- Install the *Desktop development with C++* workload of Visual Studio Build Tools and
  Microsoft Edge WebView2 (preinstalled on Windows 10/11).
- `cargo tauri build` produces an NSIS `.exe` and an `.msi` in `target\release\bundle\`.
  The installer stays small because WebView2 is downloaded by the bootstrapper if missing.

### macOS
- `xcode-select --install`
- `cargo tauri build` produces a `.app` and `.dmg`. For Apple Silicon + Intel in one go:
  `cargo tauri build --target universal-apple-darwin` (needs both targets added with rustup).

### Icons
Icons are pre-generated in `apps/desktop/src-tauri/icons/`. To regenerate from the SVG with
Icons are rendered from `assets/icon.svg` and `assets/icon-light.svg` with `python3 scripts/render-icons.py` (needs `pip install cairosvg`); the 1024 px PNG master is kept next to them.

## Auto-update (desktop)

Veranda updates itself through Tauri's signed updater. One-time setup:

1. Install the Tauri CLI, then generate a signing key pair (keep the private key outside the repo):
   ```sh
   cargo install tauri-cli --version "^2" --locked
   cargo tauri signer generate -w ~/.tauri/veranda.key
   ```
2. Put the **public** key (contents of `~/.tauri/veranda.key.pub`) into
   `apps/desktop/src-tauri/tauri.conf.json` -> `plugins.updater.pubkey`.
3. In the GitHub repository add the secrets `TAURI_SIGNING_PRIVATE_KEY` (contents of
   `~/.tauri/veranda.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (the password you chose, or empty).
4. Veranda checks `https://github.com/livrens/veranda/releases/latest/download/latest.json` on start
   and from *Check for updates* (forks can point `update.repo` in `veranda.toml` elsewhere); it
   verifies the signature, downloads, installs and restarts.

Platform notes: Windows (NSIS/MSI) and macOS (.app) update in place. On Linux only the
**AppImage** can self-update; `.deb`/`.rpm` installs show the release page instead.
If the private key is ever lost, existing installs can no longer receive updates: back it up.

## Releases (the official way: installers built by GitHub Actions)

1. Make sure these three say the same version: `VERSION`, `workspace.package.version` in `Cargo.toml`,
   `version` in `apps/desktop/src-tauri/tauri.conf.json`; add notes to `CHANGELOG.md`.
2. Commit, then `git tag v0.1.0 && git push origin v0.1.0`.
3. `.github/workflows/release.yml` builds on GitHub's machines (nothing runs on your laptop):
   Linux `.deb` / `.rpm` / `.AppImage`, Windows `.exe` (NSIS) / `.msi`, macOS universal `.dmg`,
   the CLI binaries, the signed updater files and `latest.json`. Everything lands on a **draft**
   release. Open it on GitHub, check the assets, and press *Publish release*.
4. Install once by hand: Linux `sudo apt install ./veranda_0.1.0_amd64.deb` (or run the AppImage),
   Windows run the `.exe`, macOS open the `.dmg`. Veranda then appears in the app menu.
5. From the next release on, Veranda shows an
   update banner (or installs automatically if *Install updates automatically* is on). On Linux the
   AppImage self-updates; `.deb`/`.rpm` installs show the release page instead.

Costs: GitHub Actions is free for public repos; private repos get 2000 minutes/month, and macOS
minutes count 10x. One release uses roughly 15 (Linux) + 2x20 (Windows) + 10x25 (macOS) minutes.
Remove the macOS entry from the matrix if you do not need it yet.

## Disk use

`target/` grows with every dependency bump. The dev profile keeps no debuginfo for
dependencies and only line tables for the workspace, incremental compilation is off in
`scripts/build-cool.sh` (`INCREMENTAL=1` turns it back on), and that script prunes the build
caches whenever `target/` passes `TARGET_MAX_MB` (default 20 GB). Delete `--release` and
cross-compile outputs when you are done with them; `cargo clean` is always safe.
