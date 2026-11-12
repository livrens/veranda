//! Opening URLs, folders, editors and terminals; running commands with elevation.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn open_url(cfg: &Config, url: &str) -> Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("refusing to open non-http url {url}");
    }
    // A URL is one argument for a browser, never a shell line: no whitespace, quotes or
    // control characters, which is all a command interpreter could trip over.
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\''))
    {
        bail!("refusing to open a url with spaces or quotes");
    }
    if !cfg.general.browser.trim().is_empty() {
        return spawn_quiet(crate::process::new_cmd(cfg.general.browser.trim()).arg(url));
    }
    open_with_system(url)
}

pub fn open_path(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    open_with_system(&path.to_string_lossy())
}

fn open_with_system(target: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Not `cmd /C start`: cmd.exe re-parses its command line, so `&`, `^` or `%` inside a
        // URL or folder name would run as commands. These two take the target as a plain argument.
        if target.starts_with("http://") || target.starts_with("https://") {
            spawn_quiet(
                crate::process::new_cmd("rundll32.exe")
                    .arg("url.dll,FileProtocolHandler")
                    .arg(target),
            )
        } else {
            spawn_quiet(crate::process::new_cmd("explorer.exe").arg(target))
        }
    }
    #[cfg(target_os = "macos")]
    {
        spawn_quiet(crate::process::new_cmd("open").arg(target))
    }
    #[cfg(target_os = "linux")]
    {
        spawn_quiet(crate::process::new_cmd("xdg-open").arg(target))
    }
}

pub fn open_editor(cfg: &Config, path: &Path) -> Result<()> {
    let editor = cfg.general.editor.trim();
    if editor.is_empty() {
        return open_path(path);
    }
    spawn_quiet(crate::process::new_cmd(editor).arg(path))
}

/// Open a terminal in `dir` with `path_entries` prepended to PATH.
pub fn open_terminal(dir: &Path, path_entries: &[PathBuf]) -> Result<()> {
    let path = crate::process::env_path(path_entries);
    #[cfg(target_os = "windows")]
    {
        let mut cmd = if which::which("wt.exe").is_ok() {
            let mut c = crate::process::new_cmd("wt.exe");
            c.args(["-d", &dir.to_string_lossy()]);
            c
        } else {
            let mut c = crate::process::new_cmd("cmd");
            c.args([
                "/C",
                "start",
                "",
                "cmd",
                "/K",
                &format!("cd /d \"{}\"", dir.display()),
            ]);
            c
        };
        cmd.env("PATH", path);
        spawn_quiet(&mut cmd)
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\"\n activate\n do script \"cd {} && export PATH={}\"\nend tell",
            shell_quote(&dir.to_string_lossy()),
            shell_quote(&path.to_string_lossy())
        );
        spawn_quiet(crate::process::new_cmd("osascript").args(["-e", &script]))
    }
    #[cfg(target_os = "linux")]
    {
        let candidates: [(&str, &[&str]); 7] = [
            ("x-terminal-emulator", &[]),
            ("gnome-terminal", &["--"]),
            ("konsole", &["-e"]),
            ("xfce4-terminal", &["-e"]),
            ("kitty", &[]),
            ("alacritty", &["-e"]),
            ("xterm", &["-e"]),
        ];
        for (term, args) in candidates {
            if which::which(term).is_ok() {
                let mut cmd = crate::process::new_cmd(term);
                cmd.current_dir(dir).env("PATH", &path);
                if term == "gnome-terminal" {
                    cmd.arg("--working-directory").arg(dir);
                }
                if !args.is_empty() {
                    cmd.args(args);
                    cmd.arg(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
                }
                return spawn_quiet(&mut cmd);
            }
        }
        bail!("no terminal emulator found")
    }
}

#[allow(dead_code)]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn spawn_quiet(cmd: &mut Command) -> Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .with_context(|| format!("launching {:?}", cmd.get_program()))?;
    Ok(())
}

/// Run `program args` with administrator rights using the platform's graphical prompt
/// (pkexec / osascript / UAC). Blocks until it finishes.
pub fn run_elevated(program: &Path, args: &[String]) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if which::which("pkexec").is_ok() {
            let st = crate::process::new_cmd("pkexec")
                .arg(program)
                .args(args)
                .status()?;
            if !st.success() {
                bail!("pkexec exited with {st}");
            }
            return Ok(());
        }
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let st = crate::process::new_cmd("sudo")
                .arg(program)
                .args(args)
                .status()?;
            if !st.success() {
                bail!("sudo exited with {st}");
            }
            return Ok(());
        }
        bail!("no elevation helper (pkexec) available")
    }
    #[cfg(target_os = "macos")]
    {
        let mut line = shell_quote(&program.to_string_lossy());
        for a in args {
            line.push(' ');
            line.push_str(&shell_quote(a));
        }
        let script = format!(
            "do shell script {} with administrator privileges",
            applescript_string(&line)
        );
        let out = crate::process::new_cmd("osascript")
            .args(["-e", &script])
            .output()?;
        if !out.status.success() {
            bail!("osascript failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let arg_list: Vec<String> = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect();
        let ps = format!(
            "$p = Start-Process -FilePath '{}' -ArgumentList @({}) -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
            program.to_string_lossy().replace('\'', "''"),
            arg_list.join(",")
        );
        let st = crate::process::new_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .status()?;
        if !st.success() {
            bail!("elevated command failed with {st}");
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Grant a binary permission to bind ports < 1024 on Linux without root.
#[cfg(target_os = "linux")]
pub fn grant_bind_capability(binary: &Path) -> Result<()> {
    let setcap = which::which("setcap").context("setcap not found (install libcap2-bin)")?;
    run_elevated(
        &setcap,
        &[
            "cap_net_bind_service=+ep".into(),
            binary.to_string_lossy().into_owned(),
        ],
    )
}

#[cfg(not(target_os = "linux"))]
pub fn grant_bind_capability(_binary: &Path) -> Result<()> {
    Ok(())
}

/// Linux: does the binary carry `cap_net_bind_service` (granted by `setcap`)?
#[cfg(target_os = "linux")]
pub fn has_bind_capability(binary: &Path) -> bool {
    let Ok(getcap) = which::which("getcap") else {
        return false;
    };
    crate::process::new_cmd(getcap)
        .arg(binary)
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("cap_net_bind_service"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
pub fn has_bind_capability(_binary: &Path) -> bool {
    true
}

/// A runtime installed by another tool (nvm, fnm, volta, the system package manager).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalRuntime {
    pub kind: String,
    pub version: String,
    /// Folder that holds the executable (for "open folder" and PATH).
    pub bin_dir: PathBuf,
    pub source: String,
}

fn versions_under(
    dir: &Path,
    bin_rel: &str,
    exe: &str,
    source: &str,
    out: &mut Vec<ExternalRuntime>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let version = name.trim_start_matches('v').to_string();
        if version.is_empty() || !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let bin_dir = if bin_rel.is_empty() {
            e.path()
        } else {
            e.path().join(bin_rel)
        };
        if bin_dir.join(exe).is_file() {
            out.push(ExternalRuntime {
                kind: "node".into(),
                version,
                bin_dir,
                source: source.into(),
            });
        }
    }
}

/// The user's nvm installation, if any: `$NVM_DIR` or `~/.nvm` with `nvm.sh` (unix), or
/// nvm-windows via `$NVM_HOME`/`nvm.exe` on PATH.
pub fn nvm_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        if let Some(h) = std::env::var_os("NVM_HOME").map(PathBuf::from)
            && h.join("nvm.exe").is_file()
        {
            return Some(h);
        }
        return which::which("nvm")
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    dir.join("nvm.sh").is_file().then_some(dir)
}

/// `nvm install <version>` through the user's own nvm, so the version shows up in `nvm ls`.
/// Blocks until nvm finishes; its output goes to `on_line` for progress display.
pub fn nvm_install(version: &str, on_line: &dyn Fn(String)) -> Result<()> {
    let dir = nvm_dir().context("nvm is not installed")?;
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        bail!("invalid Node version {version:?}");
    }
    let mut cmd = if cfg!(windows) {
        let mut c = crate::process::new_cmd(dir.join("nvm.exe"));
        c.args(["install", version]);
        c
    } else {
        let mut c = crate::process::new_cmd("bash");
        c.arg("-c")
            .arg(format!(
                "source \"$NVM_DIR/nvm.sh\" --no-use && nvm install {version}"
            ))
            .env("NVM_DIR", &dir);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("starting nvm")?;
    let out = child.stdout.take().context("nvm output")?;
    let err_pipe = child.stderr.take().context("nvm output")?;
    let reader = std::io::BufReader::new(out);
    use std::io::BufRead;
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        if !line.is_empty() {
            on_line(line);
        }
    }
    let mut err_text = String::new();
    std::io::Read::read_to_string(&mut std::io::BufReader::new(err_pipe), &mut err_text)?;
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "nvm install {version} failed: {}",
            err_text.lines().last().unwrap_or("").trim()
        );
    }
    Ok(())
}

/// `nvm uninstall <version>` through the user's own nvm.
/// Make `version` nvm's default (`nvm alias default`, or `nvm use` on nvm-windows), so the
/// user's own shells pick it too.
pub fn nvm_set_default(version: &str) -> Result<()> {
    let dir = nvm_dir().context("nvm is not installed")?;
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        bail!("invalid Node version {version:?}");
    }
    let mut cmd = if cfg!(windows) {
        let mut c = crate::process::new_cmd(dir.join("nvm.exe"));
        c.args(["use", version]);
        c
    } else {
        let mut c = crate::process::new_cmd("bash");
        c.arg("-c")
            .arg(format!(
                "source \"$NVM_DIR/nvm.sh\" --no-use && nvm alias default {version}"
            ))
            .env("NVM_DIR", &dir);
        c
    };
    cmd.stdin(Stdio::null());
    let out = cmd.output().context("starting nvm")?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        bail!(
            "nvm alias default {version} failed: {}",
            text.lines().last().unwrap_or("").trim()
        );
    }
    Ok(())
}

pub fn nvm_uninstall(version: &str) -> Result<()> {
    let dir = nvm_dir().context("nvm is not installed")?;
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        bail!("invalid Node version {version:?}");
    }
    let mut cmd = if cfg!(windows) {
        let mut c = crate::process::new_cmd(dir.join("nvm.exe"));
        c.args(["uninstall", version]);
        c
    } else {
        let mut c = crate::process::new_cmd("bash");
        c.arg("-c")
            .arg(format!(
                "source \"$NVM_DIR/nvm.sh\" --no-use && nvm uninstall {version}"
            ))
            .env("NVM_DIR", &dir);
        c
    };
    cmd.stdin(Stdio::null());
    let out = cmd.output().context("starting nvm")?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        bail!(
            "nvm uninstall {version} failed: {}",
            text.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
        );
    }
    Ok(())
}

/// Node.js versions managed by nvm, nvm-windows, fnm or volta. Filesystem only (no processes),
/// so it is cheap enough to run on every status refresh.
pub fn external_node_versions() -> Vec<ExternalRuntime> {
    let mut out = Vec::new();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let (bin_rel, exe) = if cfg!(windows) {
        ("", "node.exe")
    } else {
        ("bin", "node")
    };
    let nvm_dir = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    versions_under(
        &nvm_dir.join("versions").join("node"),
        bin_rel,
        exe,
        "nvm",
        &mut out,
    );
    if cfg!(windows) {
        let nvm_home = std::env::var_os("NVM_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("nvm")));
        if let Some(d) = nvm_home {
            versions_under(&d, "", exe, "nvm", &mut out);
        }
    }
    let fnm_dir = std::env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("fnm")));
    if let Some(d) = fnm_dir {
        let rel = if cfg!(windows) {
            "installation"
        } else {
            "installation/bin"
        };
        versions_under(&d.join("node-versions"), rel, exe, "fnm", &mut out);
    }
    versions_under(
        &home.join(".volta").join("tools").join("image").join("node"),
        bin_rel,
        exe,
        "volta",
        &mut out,
    );
    out.sort_by(|a, b| {
        crate::manifest::parse_version(&b.version).cmp(&crate::manifest::parse_version(&a.version))
    });
    out.dedup_by(|a, b| a.version == b.version && a.source == b.source);
    out
}

/// Our own executable, for re-invoking as an elevated helper.
pub fn current_exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?)
}
