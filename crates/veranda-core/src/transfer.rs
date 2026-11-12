//! Move to another computer: everything personal in one encrypted bundle (`.veranda-move`),
//! restored on the new machine even when it runs another operating system.
//!
//! What goes in (each part can be left out):
//! * `veranda`: configuration, sites, the local CA, remote hosts / keys / snippets, secrets.
//! * `projects`: the sites in the document root plus manually added site folders.
//! * `ssh`: `~/.ssh` (keys, config, known hosts).
//! * `git`: `~/.gitconfig`, `~/.config/git`, `~/.git-credentials`.
//! * `vscode`: VS Code user settings, keybindings, snippets and the extension list.
//! * `claude`: Claude Code settings, CLAUDE.md, skills, agents, commands, plugins, project
//!   memory and credentials (never session transcripts or caches).
//! * `folders`: any other folders you pick.
//!
//! Programs cannot move between operating systems. The bundle records which Veranda packages,
//! VS Code extensions, git repositories and developer tools were present so the new machine can
//! install the same ones (`winget` on Windows) and clone what is missing.
//!
//! Format: `VRNDMOV1` + a JSON header (salt, PBKDF2 rounds) + AES-256-GCM chunks over a gzip
//! tar stream. Each chunk is authenticated with its index and a "last" flag, so a truncated or
//! reordered file is rejected, and a wrong password fails on the first chunk.

use crate::fsutil;
use crate::paths::Layout;
use anyhow::{Context, Result, bail};
use ring::aead;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};

pub const EXTENSION: &str = "veranda-move";
const MAGIC: &[u8; 8] = b"VRNDMOV1";
const ITERATIONS: u32 = 600_000;
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
pub const MIN_PASSWORD: usize = 8;

/// Folders that are rebuilt by a package manager and never worth carrying along.
pub const HEAVY_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    ".next",
    ".nuxt",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    ".cache",
    ".turbo",
    ".parcel-cache",
];

/* ---------------- where things live on this computer ---------------- */

/// The directories a bundle reads from or writes to. Separate from the process environment so
/// tests can point everything at temporary folders.
#[derive(Debug, Clone)]
pub struct Homes {
    pub home: PathBuf,
    /// Per-user config dir: `~/.config`, `~/Library/Application Support`, `%APPDATA%`.
    pub config: PathBuf,
    pub root: PathBuf,
}

impl Homes {
    pub fn current(layout: &Layout) -> Result<Self> {
        Ok(Self {
            home: dirs::home_dir().context("no home directory")?,
            config: dirs::config_dir().context("no config directory")?,
            root: layout.root().to_path_buf(),
        })
    }
    fn layout(&self) -> Layout {
        Layout::new(self.root.clone())
    }
    fn vscode_user(&self) -> PathBuf {
        self.config.join("Code").join("User")
    }
    fn vscode_extensions(&self) -> PathBuf {
        self.home.join(".vscode").join("extensions")
    }
    fn claude_dir(&self) -> PathBuf {
        self.home.join(".claude")
    }
}

/* ---------------- manifest ---------------- */

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SectionInfo {
    pub id: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PkgRef {
    pub kind: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoRef {
    /// Where the repository will be after restore, relative to the projects folder or the home
    /// (see `base`).
    pub path: String,
    /// `projects` or `home`.
    pub base: String,
    pub remote: String,
    pub branch: String,
    /// `true` when the working copy itself is in the bundle (otherwise only clone-able).
    pub included: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolRef {
    pub id: String,
    pub label: String,
    /// Package id for `winget install --id`.
    pub winget: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtraFolder {
    /// Entry name under `folders/` in the bundle.
    pub name: String,
    /// Absolute path on the old computer.
    pub original: String,
    /// Path relative to the old home when the folder was inside it.
    pub home_relative: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub created: String,
    pub from_os: String,
    pub hostname: String,
    pub veranda_version: String,
    pub sections: Vec<SectionInfo>,
    pub packages: Vec<PkgRef>,
    pub repos: Vec<RepoRef>,
    pub vscode_extensions: Vec<String>,
    pub tools: Vec<ToolRef>,
    pub folders: Vec<ExtraFolder>,
    /// Site names in the document root that were included.
    pub sites: Vec<String>,
    /// Document root and home on the old computer, so project data keyed by path (Claude Code
    /// memory, `.claude.json`) can be relinked to the new locations.
    #[serde(default)]
    pub docroot: String,
    #[serde(default)]
    pub home: String,
}

impl Manifest {
    pub fn section(&self, id: &str) -> Option<&SectionInfo> {
        self.sections.iter().find(|s| s.id == id)
    }
    pub fn total_bytes(&self) -> u64 {
        self.sections.iter().map(|s| s.bytes).sum()
    }
}

/* ---------------- requests ---------------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateRequest {
    pub file: PathBuf,
    pub password: String,
    /// Section ids to include; empty = all standard sections.
    pub sections: Vec<String>,
    /// Extra folders (absolute paths).
    pub folders: Vec<PathBuf>,
    /// Keep `node_modules`, `vendor`, `target` and friends (see [`HEAVY_DIRS`]).
    pub include_heavy: bool,
    /// Extra files to add, as (bundle path, bytes); the desktop app uses it for exported
    /// keyring secrets.
    #[serde(skip)]
    pub extra_files: Vec<(String, Vec<u8>)>,
}

impl Default for CreateRequest {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            password: String::new(),
            sections: vec![],
            folders: vec![],
            include_heavy: false,
            extra_files: vec![],
        }
    }
}

pub const SECTIONS: &[&str] = &[
    "veranda",
    "projects",
    "databases",
    "ssh",
    "git",
    "vscode",
    "claude",
    "folders",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RestoreRequest {
    pub file: PathBuf,
    pub password: String,
    /// Section ids to restore; empty = everything in the bundle.
    pub sections: Vec<String>,
}

impl Default for RestoreRequest {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            password: String::new(),
            sections: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Report {
    pub restored: usize,
    pub bytes: u64,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    /// Existing files that were moved aside before being replaced.
    pub backup_dir: Option<String>,
    /// How many existing files were replaced (their copies are in `backup_dir`).
    pub replaced: usize,
    /// SQL dumps from the bundle, now under `backups/`, waiting to be loaded into the database.
    pub database_dumps: Vec<String>,
    pub manifest: Manifest,
    /// Repositories listed in the bundle that are not present after restore.
    pub repos_to_clone: Vec<RepoRef>,
    /// Tools from the bundle that are not on `PATH` here.
    pub tools_missing: Vec<ToolRef>,
}

/// One file (or symlink / empty dir) that will go into the bundle.
#[derive(Debug, Clone)]
struct Item {
    section: &'static str,
    bundle_path: String,
    source: PathBuf,
    size: u64,
}

/// What a create request would include, before anything is written.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Plan {
    pub sections: Vec<SectionInfo>,
    pub packages: Vec<PkgRef>,
    pub repos: Vec<RepoRef>,
    pub vscode_extensions: Vec<String>,
    pub tools: Vec<ToolRef>,
    pub sites: Vec<String>,
    pub folders: Vec<ExtraFolder>,
    pub total_bytes: u64,
    /// Left out on purpose: symlinks, folders inside the Veranda root, missing folders.
    pub skipped: Vec<String>,
    /// Things worth knowing before creating (for example which database dump is used).
    pub notes: Vec<String>,
}

/* ---------------- collecting ---------------- */

fn wanted(req_sections: &[String], id: &str) -> bool {
    req_sections.is_empty() || req_sections.iter().any(|s| s == id)
}

fn is_heavy(name: &str) -> bool {
    HEAVY_DIRS.contains(&name)
}

/// Walk `dir`, pushing every regular file. `skip` decides per directory name whether to
/// descend. Symlinks are recorded as skipped (they rarely survive a change of OS).
fn walk(
    section: &'static str,
    dir: &Path,
    prefix: &str,
    skip: &dyn Fn(&Path, &str) -> bool,
    out: &mut Vec<Item>,
    skipped: &mut Vec<String>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        let rel = format!("{prefix}/{name}");
        if meta.file_type().is_symlink() {
            skipped.push(format!("{} (symlink)", p.display()));
            continue;
        }
        if meta.is_dir() {
            if skip(&p, &name) {
                continue;
            }
            walk(section, &p, &rel, skip, out, skipped);
        } else if meta.is_file() {
            if name.ends_with(".sock") {
                continue;
            }
            out.push(Item {
                section,
                bundle_path: rel,
                source: p,
                size: meta.len(),
            });
        }
    }
}

fn push_file(section: &'static str, src: &Path, bundle_path: &str, out: &mut Vec<Item>) {
    if let Ok(meta) = std::fs::symlink_metadata(src)
        && meta.is_file()
    {
        out.push(Item {
            section,
            bundle_path: bundle_path.to_string(),
            source: src.to_path_buf(),
            size: meta.len(),
        });
    }
}

/// Read `origin` and the current branch from a `.git` folder without running git.
fn repo_info(git_dir: &Path) -> (String, String) {
    let mut remote = String::new();
    if let Ok(cfg) = std::fs::read_to_string(git_dir.join("config")) {
        let mut in_origin = false;
        for line in cfg.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_origin = l.replace(' ', "") == "[remote\"origin\"]";
            } else if in_origin && let Some(v) = l.strip_prefix("url") {
                remote = v.trim_start_matches([' ', '=']).trim().to_string();
            }
        }
    }
    let branch = std::fs::read_to_string(git_dir.join("HEAD"))
        .ok()
        .and_then(|h| {
            h.trim()
                .strip_prefix("ref: refs/heads/")
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    (remote, branch)
}

/// Find git repositories under `dir` (not descending into one once found).
fn find_repos(dir: &Path, base: &str, prefix: &str, included: bool, out: &mut Vec<RepoRef>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    if dir.join(".git").is_dir() {
        let (remote, branch) = repo_info(&dir.join(".git"));
        out.push(RepoRef {
            path: prefix.trim_start_matches('/').to_string(),
            base: base.into(),
            remote,
            branch,
            included,
        });
        return;
    }
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() && !is_heavy(&name) && !name.starts_with('.') {
            find_repos(&p, base, &format!("{prefix}/{name}"), included, out);
        }
    }
}

const TOOLS: &[(&str, &str, &str)] = &[
    ("git", "Git", "Git.Git"),
    ("gh", "GitHub CLI", "GitHub.cli"),
    ("code", "Visual Studio Code", "Microsoft.VisualStudioCode"),
    ("docker", "Docker", "Docker.DockerDesktop"),
    ("node", "Node.js (system)", "OpenJS.NodeJS.LTS"),
    ("python3", "Python", "Python.Python.3.13"),
    ("go", "Go", "GoLang.Go"),
    ("java", "Java", "EclipseAdoptium.Temurin.21.JDK"),
    ("cargo", "Rust", "Rustlang.Rustup"),
    ("claude", "Claude Code", "Anthropic.ClaudeCode"),
];

fn detect_tools() -> Vec<ToolRef> {
    TOOLS
        .iter()
        .filter(|(bin, _, _)| {
            which::which(bin).is_ok() || (*bin == "python3" && which::which("python").is_ok())
        })
        .map(|(id, label, winget)| ToolRef {
            id: id.to_string(),
            label: label.to_string(),
            winget: winget.to_string(),
        })
        .collect()
}

pub fn missing_tools(tools: &[ToolRef]) -> Vec<ToolRef> {
    tools
        .iter()
        .filter(|t| {
            which::which(&t.id).is_err() && !(t.id == "python3" && which::which("python").is_ok())
        })
        .cloned()
        .collect()
}

fn vscode_extensions(homes: &Homes) -> Vec<String> {
    let f = homes.vscode_extensions().join("extensions.json");
    let Ok(text) = std::fs::read_to_string(f) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return vec![];
    };
    let mut ids: Vec<String> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.pointer("/identifier/id").and_then(|s| s.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

/// Claude Code: keep what configures the tool, leave out transcripts, caches and sessions.
fn claude_items(homes: &Homes, out: &mut Vec<Item>, skipped: &mut Vec<String>) {
    let dir = homes.claude_dir();
    if !dir.is_dir() {
        return;
    }
    for f in [
        "settings.json",
        "settings.local.json",
        "CLAUDE.md",
        "keybindings.json",
        ".credentials.json",
        "mcp.json",
    ] {
        push_file("claude", &dir.join(f), &format!("home/.claude/{f}"), out);
    }
    for d in ["skills", "agents", "commands", "hooks", "rules"] {
        walk(
            "claude",
            &dir.join(d),
            &format!("home/.claude/{d}"),
            &|_, n| is_heavy(n),
            out,
            skipped,
        );
    }
    walk(
        "claude",
        &dir.join("plugins"),
        "home/.claude/plugins",
        &|_, n| matches!(n, "repos" | "cache" | "marketplaces") || is_heavy(n),
        out,
        skipped,
    );
    // projects/<slug>/memory and the project CLAUDE.md, never the .jsonl transcripts
    if let Ok(rd) = std::fs::read_dir(dir.join("projects")) {
        let mut ps: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        ps.sort_by_key(|e| e.file_name());
        for p in ps {
            let slug = p.file_name().to_string_lossy().to_string();
            let base = format!("home/.claude/projects/{slug}");
            walk(
                "claude",
                &p.path().join("memory"),
                &format!("{base}/memory"),
                &|_, _| false,
                out,
                skipped,
            );
            push_file(
                "claude",
                &p.path().join("CLAUDE.md"),
                &format!("{base}/CLAUDE.md"),
                out,
            );
        }
    }
    push_file(
        "claude",
        &homes.home.join(".claude.json"),
        "home/.claude.json",
        out,
    );
}

fn folder_name(p: &Path, taken: &[ExtraFolder]) -> String {
    let base = fsutil::slugify(
        &p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    let base = if base.is_empty() {
        "folder".into()
    } else {
        base
    };
    let mut name = base.clone();
    let mut i = 2;
    while taken.iter().any(|f| f.name == name) {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

struct Collected {
    items: Vec<Item>,
    skipped: Vec<String>,
    notes: Vec<String>,
    repos: Vec<RepoRef>,
    sites: Vec<String>,
    folders: Vec<ExtraFolder>,
}

fn collect(homes: &Homes, req: &CreateRequest) -> Result<Collected> {
    let layout = homes.layout();
    let cfg = crate::config::Config::load_or_default(&layout)?;
    let mut items = vec![];
    let mut skipped = vec![];
    let mut notes = vec![];
    let mut repos = vec![];
    let mut sites = vec![];
    let mut folders = vec![];
    let heavy = |_: &Path, n: &str| !req.include_heavy && is_heavy(n);

    if wanted(&req.sections, "veranda") {
        let etc = layout.etc();
        walk(
            "veranda",
            &etc,
            "veranda/etc",
            &|p, n| n == "apps" && p.parent() == Some(etc.as_path()),
            &mut items,
            &mut skipped,
        );
    }
    if wanted(&req.sections, "projects") {
        let docroot = cfg.document_root(&layout);
        for site in crate::sites::discover(&layout, &cfg)? {
            if site.auto {
                let rel = format!("projects/{}", site.name);
                walk(
                    "projects",
                    &site.path,
                    &rel,
                    &heavy,
                    &mut items,
                    &mut skipped,
                );
                find_repos(&site.path, "projects", &site.name, true, &mut repos);
                sites.push(site.name.clone());
            } else if fsutil::is_within(&docroot, &site.path) {
                // manual site inside the document root: already covered by its folder
                continue;
            } else if site.path.is_dir() {
                let name = folder_name(&site.path, &folders);
                let rel = format!("folders/{name}");
                walk(
                    "projects",
                    &site.path,
                    &rel,
                    &heavy,
                    &mut items,
                    &mut skipped,
                );
                find_repos(&site.path, "home", &name, true, &mut repos);
                folders.push(ExtraFolder {
                    name,
                    original: site.path.display().to_string(),
                    home_relative: site
                        .path
                        .strip_prefix(&homes.home)
                        .ok()
                        .map(|r| r.to_string_lossy().replace('\\', "/")),
                });
            }
        }
    }
    if wanted(&req.sections, "databases") {
        database_items(homes, &layout, &cfg, &mut items, &mut notes);
    }
    if wanted(&req.sections, "ssh") {
        walk(
            "ssh",
            &homes.home.join(".ssh"),
            "home/.ssh",
            &|_, _| false,
            &mut items,
            &mut skipped,
        );
    }
    if wanted(&req.sections, "git") {
        push_file(
            "git",
            &homes.home.join(".gitconfig"),
            "home/.gitconfig",
            &mut items,
        );
        push_file(
            "git",
            &homes.home.join(".gitignore_global"),
            "home/.gitignore_global",
            &mut items,
        );
        push_file(
            "git",
            &homes.home.join(".git-credentials"),
            "home/.git-credentials",
            &mut items,
        );
        walk(
            "git",
            &homes.config.join("git"),
            "home/.config/git",
            &|_, _| false,
            &mut items,
            &mut skipped,
        );
    }
    if wanted(&req.sections, "vscode") {
        let user = homes.vscode_user();
        for f in ["settings.json", "keybindings.json", "tasks.json"] {
            push_file(
                "vscode",
                &user.join(f),
                &format!("vscode/User/{f}"),
                &mut items,
            );
        }
        walk(
            "vscode",
            &user.join("snippets"),
            "vscode/User/snippets",
            &|_, _| false,
            &mut items,
            &mut skipped,
        );
    }
    if wanted(&req.sections, "claude") {
        claude_items(homes, &mut items, &mut skipped);
    }
    if wanted(&req.sections, "folders") {
        for f in &req.folders {
            if !f.is_dir() {
                skipped.push(format!("{} (not a folder)", f.display()));
                continue;
            }
            if fsutil::is_within(&homes.root, f) {
                skipped.push(format!("{} (inside the Veranda root)", f.display()));
                continue;
            }
            let name = folder_name(f, &folders);
            walk(
                "folders",
                f,
                &format!("folders/{name}"),
                &heavy,
                &mut items,
                &mut skipped,
            );
            find_repos(f, "home", &name, true, &mut repos);
            folders.push(ExtraFolder {
                name,
                original: f.display().to_string(),
                home_relative: f
                    .strip_prefix(&homes.home)
                    .ok()
                    .map(|r| r.to_string_lossy().replace('\\', "/")),
            });
        }
    }
    Ok(Collected {
        items,
        skipped,
        notes,
        repos,
        sites,
        folders,
    })
}

/// A fresh dump of the user databases when the server is running, otherwise the newest SQL
/// backup Veranda made earlier (if any).
fn database_items(
    homes: &Homes,
    layout: &Layout,
    cfg: &crate::config::Config,
    out: &mut Vec<Item>,
    notes: &mut Vec<String>,
) {
    let app = crate::Veranda {
        layout: layout.clone(),
        config: cfg.clone(),
    };
    let fresh = crate::packages::Registry::load(layout)
        .ok()
        .and_then(|reg| crate::db::DbCtx::from_app(&app, &reg))
        .filter(|db| crate::process::port_accepting(db.port))
        .and_then(|db| {
            let dest = layout.tmp().join("move-databases.sql");
            match crate::db::dump_user_databases(&db, &dest) {
                Ok(names) => {
                    notes.push(if names.is_empty() {
                        "databases: the server has no user databases".into()
                    } else {
                        format!("databases: fresh dump of {}", names.join(", "))
                    });
                    Some((db.engine.id().to_string(), dest))
                }
                Err(e) => {
                    notes.push(format!(
                        "databases: dump failed ({e}); using the newest backup"
                    ));
                    None
                }
            }
        });
    let _ = homes;
    let (name, source) = match fresh {
        Some((engine, dest)) => (format!("{engine}-{}.sql", fsutil::timestamp()), dest),
        None => {
            let newest = std::fs::read_dir(layout.backups())
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| crate::backup::is_dump(&e.path()))
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            match newest {
                Some(e) => {
                    let n = e.file_name().to_string_lossy().to_string();
                    if !notes.iter().any(|n| n.starts_with("databases:")) {
                        notes.push(format!(
                            "databases: server not running; using the backup {n}"
                        ));
                    }
                    (n, e.path())
                }
                None => {
                    notes.push("databases: server not running and no backup found".into());
                    return;
                }
            }
        }
    };
    push_file("databases", &source, &format!("databases/{name}"), out);
}

fn section_totals(items: &[Item]) -> Vec<SectionInfo> {
    let mut map: BTreeMap<&str, SectionInfo> = BTreeMap::new();
    for it in items {
        let e = map.entry(it.section).or_insert_with(|| SectionInfo {
            id: it.section.into(),
            ..Default::default()
        });
        e.files += 1;
        e.bytes += it.size;
    }
    SECTIONS.iter().filter_map(|s| map.remove(s)).collect()
}

fn installed_packages(layout: &Layout) -> Vec<PkgRef> {
    crate::packages::Registry::load(layout)
        .map(|r| {
            r.items
                .iter()
                .map(|i| PkgRef {
                    kind: i.kind.id().to_string(),
                    version: i.version.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Size up a request without writing anything.
pub fn plan(homes: &Homes, req: &CreateRequest) -> Result<Plan> {
    let c = collect(homes, req)?;
    let sections = section_totals(&c.items);
    Ok(Plan {
        total_bytes: sections.iter().map(|s| s.bytes).sum(),
        sections,
        packages: installed_packages(&homes.layout()),
        repos: c.repos,
        vscode_extensions: vscode_extensions(homes),
        tools: detect_tools(),
        sites: c.sites,
        folders: c.folders,
        skipped: c.skipped,
        notes: c.notes,
    })
}

/* ---------------- crypto framing ---------------- */

#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    salt: String,
    iterations: u32,
}

fn derive(password: &str, salt: &[u8], iterations: u32) -> aead::LessSafeKey {
    let mut raw = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations.max(1)).expect("non-zero"),
        salt,
        password.as_bytes(),
        &mut raw,
    );
    let k = aead::UnboundKey::new(&aead::AES_256_GCM, &raw).expect("key length");
    aead::LessSafeKey::new(k)
}

fn nonce_for(index: u64) -> aead::Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&index.to_be_bytes());
    aead::Nonce::assume_unique_for_key(n)
}

fn aad_for(index: u64, last: bool) -> [u8; 9] {
    let mut a = [0u8; 9];
    a[..8].copy_from_slice(&index.to_be_bytes());
    a[8] = u8::from(last);
    a
}

struct SealWriter<W: Write> {
    inner: W,
    key: aead::LessSafeKey,
    buf: Vec<u8>,
    index: u64,
}

impl<W: Write> SealWriter<W> {
    fn seal_chunk(&mut self, last: bool) -> std::io::Result<()> {
        let mut data = std::mem::take(&mut self.buf);
        self.key
            .seal_in_place_append_tag(
                nonce_for(self.index),
                aead::Aad::from(aad_for(self.index, last)),
                &mut data,
            )
            .map_err(|_| std::io::Error::other("encrypting"))?;
        self.inner.write_all(&(data.len() as u32).to_be_bytes())?;
        self.inner.write_all(&[u8::from(last)])?;
        self.inner.write_all(&data)?;
        self.index += 1;
        self.buf = data;
        self.buf.clear();
        Ok(())
    }
    fn finish(mut self) -> std::io::Result<W> {
        self.seal_chunk(true)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = CHUNK - self.buf.len();
        let n = room.min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == CHUNK {
            self.seal_chunk(false)?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct OpenReader<R: Read> {
    inner: R,
    key: aead::LessSafeKey,
    buf: Vec<u8>,
    pos: usize,
    index: u64,
    done: bool,
}

impl<R: Read> OpenReader<R> {
    fn next_chunk(&mut self) -> std::io::Result<()> {
        let mut head = [0u8; 5];
        if let Err(e) = self.inner.read_exact(&mut head) {
            return Err(if e.kind() == std::io::ErrorKind::UnexpectedEof {
                std::io::Error::other("the bundle is truncated (download or copy incomplete?)")
            } else {
                e
            });
        }
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let last = head[4] == 1;
        if !(TAG..=CHUNK + TAG).contains(&len) {
            return Err(std::io::Error::other("damaged bundle (bad chunk length)"));
        }
        let mut data = vec![0u8; len];
        self.inner.read_exact(&mut data).map_err(|_| {
            std::io::Error::other("the bundle is truncated (download or copy incomplete?)")
        })?;
        let plain_len = self
            .key
            .open_in_place(
                nonce_for(self.index),
                aead::Aad::from(aad_for(self.index, last)),
                &mut data,
            )
            .map_err(|_| {
                std::io::Error::other(if self.index == 0 {
                    "wrong password or damaged bundle"
                } else {
                    "damaged bundle (authentication failed)"
                })
            })?
            .len();
        data.truncate(plain_len);
        self.buf = data;
        self.pos = 0;
        self.index += 1;
        self.done = last;
        Ok(())
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn open_bundle(
    file: &Path,
    password: &str,
) -> Result<OpenReader<std::io::BufReader<std::fs::File>>> {
    let f = std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
    let mut r = std::io::BufReader::new(f);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)
        .context("reading the bundle header")?;
    if &magic != MAGIC {
        bail!("not a Veranda move bundle");
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 4096 {
        bail!("damaged bundle header");
    }
    let mut hdr = vec![0u8; len];
    r.read_exact(&mut hdr)?;
    let hdr: Header = serde_json::from_slice(&hdr).context("bundle header")?;
    if hdr.version != 1 {
        bail!("bundle version {} needs a newer Veranda", hdr.version);
    }
    let salt = hex::decode(&hdr.salt).context("bundle salt")?;
    Ok(OpenReader {
        inner: r,
        key: derive(password, &salt, hdr.iterations),
        buf: vec![],
        pos: 0,
        index: 0,
        done: false,
    })
}

/* ---------------- create ---------------- */

/// A folder that looks like a project (git repository, Composer or npm package).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    pub path: String,
    pub name: String,
    /// `git`, `composer`, `node` or `project`.
    pub kind: String,
}

fn looks_like_project(dir: &Path) -> Option<&'static str> {
    if dir.join(".git").is_dir() {
        Some("git")
    } else if dir.join("composer.json").is_file() {
        Some("composer")
    } else if dir.join("package.json").is_file() {
        Some("node")
    } else if dir.join(".vscode").is_dir()
        || dir.join(".claude").is_dir()
        || dir.join("CLAUDE.md").is_file()
    {
        Some("project")
    } else {
        None
    }
}

fn suggest_in(dir: &Path, depth: u32, skip_root: &Path, out: &mut Vec<Suggestion>) {
    if out.len() >= 400 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || is_heavy(&name) || !p.is_dir() || p == skip_root {
            continue;
        }
        if std::fs::symlink_metadata(&p)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        if let Some(kind) = looks_like_project(&p) {
            out.push(Suggestion {
                path: p.display().to_string(),
                name,
                kind: kind.into(),
            });
        } else if depth > 1 {
            suggest_in(&p, depth - 1, skip_root, out);
        }
    }
}

/// Project folders under the home (three levels deep) and, on Linux, under mounted volumes.
pub fn suggest_folders(homes: &Homes) -> Vec<Suggestion> {
    suggest_folders_in(homes, true)
}

/// [`suggest_folders`] with the mounted-volume scan switchable (tests keep it off).
pub fn suggest_folders_in(homes: &Homes, volumes: bool) -> Vec<Suggestion> {
    let mut out = vec![];
    suggest_in(&homes.home, 3, &homes.root, &mut out);
    if !volumes {
        return out;
    }
    #[cfg(target_os = "linux")]
    {
        let user = homes
            .home
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        for base in [
            PathBuf::from("/run/media").join(&user),
            PathBuf::from("/media").join(&user),
            PathBuf::from("/mnt"),
        ] {
            if let Ok(rd) = std::fs::read_dir(&base) {
                for v in rd.filter_map(|e| e.ok()) {
                    if v.path().is_dir() {
                        suggest_in(&v.path(), 3, &homes.root, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// Claude Code keeps per-project data under `~/.claude/projects/<slug>` where the slug is the
/// absolute path with every non-alphanumeric character turned into `-`.
pub fn claude_slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else if ft.is_file() {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

/// Old path -> new path for every project or folder that moved with the bundle.
fn moved_paths(homes: &Homes, manifest: &Manifest, docroot: &Path) -> Vec<(String, PathBuf)> {
    let mut v = vec![];
    if !manifest.docroot.is_empty() {
        let old_root = manifest.docroot.trim_end_matches(['/', '\\']);
        for name in &manifest.sites {
            v.push((format!("{old_root}/{name}"), docroot.join(name)));
        }
    }
    for f in &manifest.folders {
        let new = match &f.home_relative {
            Some(r) => homes.home.join(r),
            None => homes.home.join("Veranda-moved").join(&f.name),
        };
        v.push((f.original.clone(), new));
    }
    v
}

/// Make Claude Code find its per-project memory and settings at the new paths: copy
/// `~/.claude/projects/<old slug>` to the new slug and rename the keys in `~/.claude.json`.
fn relink_claude(homes: &Homes, manifest: &Manifest, docroot: &Path, report: &mut Report) {
    let moved = moved_paths(homes, manifest, docroot);
    if moved.is_empty() {
        return;
    }
    let projects = homes.claude_dir().join("projects");
    for (old, new) in &moved {
        let new_s = new.display().to_string();
        if *old == new_s {
            continue;
        }
        let (from, to) = (
            projects.join(claude_slug(old)),
            projects.join(claude_slug(&new_s)),
        );
        if from.is_dir()
            && !to.exists()
            && let Err(e) = copy_dir(&from, &to)
        {
            report
                .failed
                .push(format!("Claude Code data for {new_s}: {e}"));
        }
    }
    let cfg = homes.home.join(".claude.json");
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        return;
    };
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let mut changed = false;
    if let Some(map) = json.get_mut("projects").and_then(|p| p.as_object_mut()) {
        for (old, new) in &moved {
            let new_s = new.display().to_string();
            if *old != new_s
                && !map.contains_key(&new_s)
                && let Some(v) = map.remove(old)
            {
                map.insert(new_s, v);
                changed = true;
            }
        }
    }
    if changed {
        match serde_json::to_vec_pretty(&json) {
            Ok(bytes) => {
                if let Err(e) = fsutil::write_private(&cfg, &bytes) {
                    report.failed.push(format!(".claude.json: {e}"));
                }
            }
            Err(e) => report.failed.push(format!(".claude.json: {e}")),
        }
    }
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

/// Write the bundle. `progress` gets one line per section and every few hundred files.
pub fn create(homes: &Homes, req: &CreateRequest, progress: &dyn Fn(String)) -> Result<Manifest> {
    create_with(homes, req, progress, ITERATIONS)
}

pub fn create_with(
    homes: &Homes,
    req: &CreateRequest,
    progress: &dyn Fn(String),
    iterations: u32,
) -> Result<Manifest> {
    if req.password.chars().count() < MIN_PASSWORD {
        bail!("the password needs at least {MIN_PASSWORD} characters");
    }
    if req.file.as_os_str().is_empty() {
        bail!("choose where to save the bundle");
    }
    let c = collect(homes, req)?;
    for s in c.skipped.iter().chain(c.notes.iter()) {
        tracing::info!("move bundle: {s}");
    }
    let sections = section_totals(&c.items);
    let manifest = Manifest {
        version: 1,
        created: fsutil::timestamp(),
        from_os: std::env::consts::OS.into(),
        hostname: host_name(),
        veranda_version: crate::VERSION.into(),
        sections,
        packages: installed_packages(&homes.layout()),
        repos: c.repos,
        vscode_extensions: vscode_extensions(homes),
        tools: detect_tools(),
        folders: c.folders,
        sites: c.sites,
        docroot: crate::config::Config::load_or_default(&homes.layout())
            .map(|c| c.document_root(&homes.layout()).display().to_string())
            .unwrap_or_default(),
        home: homes.home.display().to_string(),
    };

    let mut salt = vec![0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("no random bytes"))?;
    let key = derive(&req.password, &salt, iterations);
    if let Some(p) = req.file.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = req.file.with_extension(format!("{EXTENSION}.part"));
    let result: Result<()> = (|| {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAGIC)?;
        let hdr = serde_json::to_vec(&Header {
            version: 1,
            salt: hex::encode(&salt),
            iterations,
        })?;
        f.write_all(&(hdr.len() as u32).to_be_bytes())?;
        f.write_all(&hdr)?;
        let sealed = SealWriter {
            inner: f,
            key,
            buf: Vec::with_capacity(CHUNK),
            index: 0,
        };
        let gz = flate2::write::GzEncoder::new(sealed, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        tar.follow_symlinks(false);
        let mj = serde_json::to_vec_pretty(&manifest)?;
        append_bytes(&mut tar, "manifest.json", &mj)?;
        for (path, bytes) in &req.extra_files {
            append_bytes(&mut tar, path, bytes)?;
        }
        let mut current = "";
        for (i, it) in c.items.iter().enumerate() {
            if it.section != current {
                current = it.section;
                progress(format!("packing {current}…"));
            }
            if i % 300 == 0 && i > 0 {
                progress(format!("{i} of {} files", c.items.len()));
            }
            let mut src = match std::fs::File::open(&it.source) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("skipping {}: {e}", it.source.display());
                    continue;
                }
            };
            let meta = src.metadata()?;
            let mut h = tar::Header::new_gnu();
            h.set_size(meta.len());
            h.set_mode(unix_mode(&meta));
            h.set_mtime(
                meta.modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            h.set_cksum();
            tar.append_data(&mut h, &it.bundle_path, &mut src)?;
        }
        progress("finishing…".into());
        let gz = tar.into_inner()?;
        let sealed = gz.finish()?;
        let f = sealed.finish()?;
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &req.file)?;
    fsutil::make_private_file(&req.file)?;
    Ok(manifest)
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(bytes.len() as u64);
    h.set_mode(0o600);
    h.set_mtime(0);
    h.set_cksum();
    tar.append_data(&mut h, path, bytes)?;
    Ok(())
}

#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

/* ---------------- inspect / restore ---------------- */

/// Read only the manifest (checks the password on the way).
pub fn inspect(file: &Path, password: &str) -> Result<Manifest> {
    let r = open_bundle(file, password)?;
    let gz = flate2::read::GzDecoder::new(r);
    let mut tar = tar::Archive::new(gz);
    let mut entries = tar.entries()?;
    let mut first = entries
        .next()
        .context("empty bundle")?
        .context("reading the bundle")?;
    if first.path()?.to_string_lossy() != "manifest.json" {
        bail!("damaged bundle (manifest missing)");
    }
    let mut text = String::new();
    first.read_to_string(&mut text)?;
    serde_json::from_str(&text).context("bundle manifest")
}

fn safe_rel(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Where a bundle entry lands on this computer, with its section id.
fn destination(homes: &Homes, manifest: &Manifest, entry: &str) -> Option<(&'static str, PathBuf)> {
    let rel = safe_rel(Path::new(entry))?;
    let mut parts = rel.iter().map(|s| s.to_string_lossy().to_string());
    let top = parts.next()?;
    let rest: PathBuf = parts.collect();
    match top.as_str() {
        "veranda" => Some(("veranda", homes.root.join(rest))),
        "projects" => {
            let cfg = crate::config::Config::load_or_default(&homes.layout()).ok()?;
            Some(("projects", cfg.document_root(&homes.layout()).join(rest)))
        }
        "vscode" => Some(("vscode", homes.config.join("Code").join(rest))),
        "databases" => Some(("databases", homes.layout().backups().join(rest))),
        "folders" => {
            let name = rest.iter().next()?.to_string_lossy().to_string();
            let inner: PathBuf = rest.iter().skip(1).collect();
            let f = manifest.folders.iter().find(|f| f.name == name)?;
            let base = match &f.home_relative {
                Some(r) => homes.home.join(r),
                None => homes.home.join("Veranda-moved").join(&f.name),
            };
            Some(("folders", base.join(inner)))
        }
        "home" => {
            let first = rest.iter().next()?.to_string_lossy().to_string();
            let section: &'static str = match first.as_str() {
                ".ssh" => "ssh",
                ".gitconfig" | ".gitignore_global" | ".git-credentials" => "git",
                ".config" => "git",
                ".claude" | ".claude.json" => "claude",
                _ => return None,
            };
            if first == ".config" {
                // home/.config/git/... -> the platform config dir
                let inner: PathBuf = rest.iter().skip(1).collect();
                return Some((section, homes.config.join(inner)));
            }
            Some((section, homes.home.join(rest)))
        }
        _ => None,
    }
}

fn is_secret_path(entry: &str) -> bool {
    entry.starts_with("home/.ssh/")
        || entry.starts_with("veranda/etc/secrets/")
        || entry.starts_with("veranda/etc/remote/")
        || entry.starts_with("veranda/etc/ssl/")
        || entry.ends_with(".credentials.json")
        || entry.ends_with(".git-credentials")
        || entry == "home/.claude.json"
}

/// Unpack the selected sections. Existing files are moved to `backups/move-<time>/` first.
pub fn restore(homes: &Homes, req: &RestoreRequest, progress: &dyn Fn(String)) -> Result<Report> {
    let manifest = inspect(&req.file, &req.password)?;
    let r = open_bundle(&req.file, &req.password)?;
    let gz = flate2::read::GzDecoder::new(r);
    let mut tar = tar::Archive::new(gz);
    let backup_root = homes
        .layout()
        .backups()
        .join(format!("move-{}", fsutil::timestamp()));
    let mut report = Report {
        manifest: manifest.clone(),
        ..Default::default()
    };
    let mut backed_up = false;
    let mut current = "";
    for (i, entry) in tar.entries()?.enumerate() {
        let mut entry = entry.context("reading the bundle")?;
        let path = entry.path()?.to_string_lossy().to_string();
        if path == "manifest.json" {
            continue;
        }
        let Some((section, dest)) = destination(homes, &manifest, &path) else {
            report.skipped.push(format!("{path} (unknown entry)"));
            continue;
        };
        if !wanted(&req.sections, section) {
            continue;
        }
        if section != current {
            current = section;
            progress(format!("restoring {section}…"));
        }
        if i % 300 == 0 && i > 0 {
            progress(format!("{i} files…"));
        }
        if !entry.header().entry_type().is_file() {
            report.skipped.push(format!("{path} (not a regular file)"));
            continue;
        }
        let res: Result<()> = (|| {
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            if dest.exists() {
                let rel = dest.strip_prefix(&homes.home).unwrap_or(&dest);
                let rel = rel.strip_prefix(&homes.root).unwrap_or(rel);
                let rel = safe_rel(rel).unwrap_or_else(|| PathBuf::from("file"));
                let b = backup_root.join(rel);
                if let Some(p) = b.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::rename(&dest, &b).or_else(|_| {
                    std::fs::copy(&dest, &b).map(|_| ())?;
                    std::fs::remove_file(&dest)
                })?;
                backed_up = true;
                report.replaced += 1;
            }
            let mut out = std::fs::File::create(&dest)?;
            let n = std::io::copy(&mut entry, &mut out)?;
            report.bytes += n;
            if is_secret_path(&path) {
                fsutil::make_private_file(&dest)?;
                if let Some(p) = dest.parent() {
                    let _ = fsutil::make_private_dir(p);
                }
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
                    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode));
                }
            }
            Ok(())
        })();
        match res {
            Ok(()) => {
                report.restored += 1;
                if section == "databases" {
                    report.database_dumps.push(dest.display().to_string());
                }
            }
            Err(e) => report.failed.push(format!("{path}: {e}")),
        }
    }
    if backed_up {
        report.backup_dir = Some(backup_root.display().to_string());
    }
    // what still needs doing
    let docroot = crate::config::Config::load_or_default(&homes.layout())
        .map(|c| c.document_root(&homes.layout()))
        .unwrap_or_else(|_| homes.layout().www());
    for repo in &manifest.repos {
        let target = repo_target(homes, &manifest, &docroot, repo);
        if !target.join(".git").is_dir() && !repo.remote.is_empty() {
            report.repos_to_clone.push(repo.clone());
        }
    }
    report.tools_missing = missing_tools(&manifest.tools);
    if wanted(&req.sections, "claude") {
        relink_claude(homes, &manifest, &docroot, &mut report);
    }
    Ok(report)
}

/// Where a repository from the bundle should be on this computer.
pub fn repo_target(homes: &Homes, manifest: &Manifest, docroot: &Path, repo: &RepoRef) -> PathBuf {
    if repo.base == "projects" {
        return docroot.join(&repo.path);
    }
    let mut parts = repo.path.splitn(2, '/');
    let name = parts.next().unwrap_or_default();
    let inner = parts.next().unwrap_or_default();
    let base = match manifest.folders.iter().find(|f| f.name == name) {
        Some(ExtraFolder {
            home_relative: Some(r),
            ..
        }) => homes.home.join(r),
        _ => homes.home.join("Veranda-moved").join(name),
    };
    if inner.is_empty() {
        base
    } else {
        base.join(inner)
    }
}

/// Load SQL dumps into Veranda's database, starting it first when needed. Returns what was
/// loaded and what failed (with the file path, so it can be loaded by hand later).
pub fn load_database_dumps(
    app: &crate::Veranda,
    dumps: &[String],
    progress: &dyn Fn(String),
) -> (Vec<String>, Vec<String>) {
    let (mut ok, mut failed) = (vec![], vec![]);
    let reg = match crate::packages::Registry::load(&app.layout) {
        Ok(r) => r,
        Err(e) => return (ok, vec![format!("database: {e}")]),
    };
    let Some(db) = crate::db::DbCtx::from_app(app, &reg) else {
        return (
            ok,
            dumps
                .iter()
                .map(|d| format!("{d}: install and start the database, then `veranda db import`"))
                .collect(),
        );
    };
    if !crate::process::port_accepting(db.port) {
        progress("starting the database…".into());
        if let Err(e) = crate::services::start(app, crate::services::ServiceId::Db) {
            return (
                ok,
                dumps
                    .iter()
                    .map(|d| format!("{d}: database not started: {e}"))
                    .collect(),
            );
        }
        if !crate::process::wait_for_port(db.port, std::time::Duration::from_secs(40)) {
            return (
                ok,
                dumps
                    .iter()
                    .map(|d| {
                        format!(
                            "{d}: the database did not come up; load it with `veranda db import`"
                        )
                    })
                    .collect(),
            );
        }
    }
    for d in dumps {
        progress(format!("loading {d}…"));
        match crate::db::import_file(&db, Path::new(d)) {
            Ok(()) => ok.push(format!("database dump {d}")),
            Err(e) => failed.push(format!("{d}: {e}")),
        }
    }
    (ok, failed)
}

/// `git clone` every repository that is still missing. Needs git on `PATH`.
pub fn clone_repos(
    homes: &Homes,
    manifest: &Manifest,
    repos: &[RepoRef],
    progress: &dyn Fn(String),
) -> Vec<String> {
    let mut failed = vec![];
    let Ok(git) = which::which("git") else {
        return repos
            .iter()
            .map(|r| format!("{}: git is not installed", r.path))
            .collect();
    };
    let docroot = crate::config::Config::load_or_default(&homes.layout())
        .map(|c| c.document_root(&homes.layout()))
        .unwrap_or_else(|_| homes.layout().www());
    for repo in repos {
        let target = repo_target(homes, manifest, &docroot, repo);
        if target.join(".git").is_dir() {
            continue;
        }
        progress(format!("cloning {}…", repo.remote));
        if let Some(p) = target.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut cmd = crate::process::new_cmd(&git);
        cmd.arg("clone");
        if !repo.branch.is_empty() {
            cmd.args(["--branch", &repo.branch]);
        }
        cmd.arg(&repo.remote).arg(&target);
        match cmd.output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => failed.push(format!(
                "{}: {}",
                repo.path,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => failed.push(format!("{}: {e}", repo.path)),
        }
    }
    failed
}

/// Install VS Code extensions with the `code` CLI. Returns the ones that failed.
pub fn install_vscode_extensions(ids: &[String], progress: &dyn Fn(String)) -> Vec<String> {
    let Ok(code) = which::which("code").or_else(|_| which::which("code.cmd")) else {
        return ids
            .iter()
            .map(|i| format!("{i}: VS Code is not on PATH"))
            .collect();
    };
    let mut failed = vec![];
    for id in ids {
        progress(format!("VS Code extension {id}…"));
        match crate::process::new_cmd(&code)
            .args(["--install-extension", id, "--force"])
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => failed.push(format!(
                "{id}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => failed.push(format!("{id}: {e}")),
        }
    }
    failed
}

/// Windows only: `winget install` each tool. Returns the ones that failed.
pub fn winget_install(tools: &[ToolRef], progress: &dyn Fn(String)) -> Vec<String> {
    let Ok(winget) = which::which("winget") else {
        return tools
            .iter()
            .map(|t| format!("{}: winget is not available", t.label))
            .collect();
    };
    let mut failed = vec![];
    for t in tools {
        progress(format!("installing {}…", t.label));
        match crate::process::new_cmd(&winget)
            .args([
                "install",
                "--id",
                &t.winget,
                "-e",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--silent",
            ])
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => failed.push(format!(
                "{}: {}",
                t.label,
                String::from_utf8_lossy(&o.stdout).trim()
            )),
            Err(e) => failed.push(format!("{}: {e}", t.label)),
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn bundle_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let old = Homes {
            home: dir.path().join("old-home"),
            config: dir.path().join("old-home/.config"),
            root: dir.path().join("old-root"),
        };
        let layout = Layout::new(&old.root);
        layout.ensure().unwrap();
        write(&layout.config_file(), "[general]\ntld = \"test\"\n");
        write(&layout.etc().join("secrets/db-root.txt"), "s3cret");
        write(&old.home.join(".ssh/id_ed25519"), "PRIVATE");
        write(&old.home.join(".ssh/config"), "Host x\n");
        write(&old.home.join(".gitconfig"), "[user]\nname = L\n");
        write(&old.config.join("Code/User/settings.json"), "{\"a\":1}");
        write(&old.config.join("Code/User/snippets/php.json"), "{}");
        write(
            &old.home.join(".vscode/extensions/extensions.json"),
            r#"[{"identifier":{"id":"ms-python.python"}},{"identifier":{"id":"anthropic.claude-code"}}]"#,
        );
        write(&old.home.join(".claude/settings.json"), "{}");
        write(&old.home.join(".claude/CLAUDE.md"), "# me");
        write(&old.home.join(".claude/skills/x/SKILL.md"), "skill");
        write(
            &old.home.join(".claude/projects/p1/memory/MEMORY.md"),
            "mem",
        );
        write(
            &old.home.join(".claude/projects/p1/abc.jsonl"),
            "transcript",
        );
        // a site with a git repo and a heavy folder
        let site = layout.www().join("shop");
        write(&site.join("index.php"), "<?php");
        write(&site.join("node_modules/x/index.js"), "heavy");
        write(
            &site.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/x/shop.git\n",
        );
        write(&site.join(".git/HEAD"), "ref: refs/heads/main\n");
        // an extra folder under the home
        write(&old.home.join("Kerja/notes/todo.txt"), "todo");
        write(
            &layout.backups().join("mariadb-20261101-000000.sql"),
            "CREATE DATABASE shop;",
        );
        write(
            &old.home.join("Kerja/app/.git/HEAD"),
            "ref: refs/heads/dev\n",
        );
        write(&old.home.join("Kerja/lib/package.json"), "{}");
        let notes_old = old.home.join("Kerja/notes").display().to_string();
        write(
            &old.home
                .join(".claude/projects")
                .join(claude_slug(&notes_old))
                .join("memory/MEMORY.md"),
            "notes-mem",
        );
        write(
            &old.home.join(".claude.json"),
            &format!(
                "{{\"projects\":{{{:?}:{{\"allowedTools\":[]}}}}}}",
                notes_old
            ),
        );
        let sug = suggest_folders_in(&old, false);
        assert_eq!(
            sug.iter()
                .map(|s| (s.name.as_str(), s.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![("app", "git"), ("lib", "node")]
        );

        let file = dir.path().join("move.veranda-move");
        let req = CreateRequest {
            file: file.clone(),
            password: "correct horse".into(),
            folders: vec![old.home.join("Kerja/notes")],
            extra_files: vec![(
                "veranda/etc/remote/secrets-export.json".into(),
                b"{}".to_vec(),
            )],
            ..Default::default()
        };
        assert!(
            create_with(
                &old,
                &CreateRequest {
                    password: "short".into(),
                    ..req.clone()
                },
                &|_| {},
                1000
            )
            .is_err()
        );
        let p = plan(&old, &req).unwrap();
        assert_eq!(p.sites, vec!["shop"]);
        assert_eq!(p.repos.len(), 1);
        assert_eq!(p.repos[0].remote, "https://github.com/x/shop.git");
        assert_eq!(p.vscode_extensions.len(), 2);
        let m = create_with(&old, &req, &|_| {}, 1000).unwrap();
        assert_eq!(
            m.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec![
                "veranda",
                "projects",
                "databases",
                "ssh",
                "git",
                "vscode",
                "claude",
                "folders"
            ]
        );
        assert_eq!(m.folders[0].home_relative.as_deref(), Some("Kerja/notes"));

        assert!(inspect(&file, "wrong password").is_err());
        let back = inspect(&file, "correct horse").unwrap();
        assert_eq!(back, m);

        // truncated copy is rejected
        let bytes = std::fs::read(&file).unwrap();
        let cut = dir.path().join("cut.veranda-move");
        std::fs::write(&cut, &bytes[..bytes.len() - 20]).unwrap();
        let new_tmp = Homes {
            home: dir.path().join("tmp-home"),
            config: dir.path().join("tmp-home/.config"),
            root: dir.path().join("tmp-root"),
        };
        Layout::new(&new_tmp.root).ensure().unwrap();
        let err = restore(
            &new_tmp,
            &RestoreRequest {
                file: cut,
                password: "correct horse".into(),
                sections: vec![],
            },
            &|_| {},
        );
        assert!(err.is_err() || !err.unwrap().failed.is_empty());

        let new = Homes {
            home: dir.path().join("new-home"),
            config: dir.path().join("new-home/AppData"),
            root: dir.path().join("new-root"),
        };
        Layout::new(&new.root).ensure().unwrap();
        write(&new.home.join(".gitconfig"), "[user]\nname = old\n"); // will be backed up
        let rep = restore(
            &new,
            &RestoreRequest {
                file: file.clone(),
                password: "correct horse".into(),
                sections: vec![],
            },
            &|_| {},
        )
        .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(
            std::fs::read_to_string(new.home.join(".ssh/id_ed25519")).unwrap(),
            "PRIVATE"
        );
        assert_eq!(
            std::fs::read_to_string(new.home.join(".gitconfig")).unwrap(),
            "[user]\nname = L\n"
        );
        assert_eq!(
            std::fs::read_to_string(new.config.join("Code/User/settings.json")).unwrap(),
            "{\"a\":1}"
        );
        assert!(new.config.join("Code/User/snippets/php.json").is_file());
        assert!(new.home.join(".claude/skills/x/SKILL.md").is_file());
        assert!(
            new.home
                .join(".claude/projects/p1/memory/MEMORY.md")
                .is_file()
        );
        assert!(!new.home.join(".claude/projects/p1/abc.jsonl").exists());
        assert!(new.home.join(".claude.json").is_file());
        assert!(new.root.join("etc/secrets/db-root.txt").is_file());
        assert!(new.root.join("etc/remote/secrets-export.json").is_file());
        assert!(new.root.join("www/shop/index.php").is_file());
        assert_eq!(rep.database_dumps.len(), 1);
        assert!(
            new.root
                .join("backups/mariadb-20261101-000000.sql")
                .is_file()
        );
        assert_eq!(rep.replaced, 1, "only the old gitconfig existed");
        assert!(!new.root.join("www/shop/node_modules").exists());
        assert!(new.root.join("www/shop/.git/HEAD").is_file());
        assert!(new.home.join("Kerja/notes/todo.txt").is_file());
        let b = rep.backup_dir.clone().expect("old gitconfig moved aside");
        assert_eq!(
            std::fs::read_to_string(Path::new(&b).join(".gitconfig")).unwrap(),
            "[user]\nname = old\n"
        );
        assert!(rep.repos_to_clone.is_empty(), "working copy was included");
        // Claude Code project data follows the folder to its new path
        let notes_new = new.home.join("Kerja/notes").display().to_string();
        assert_eq!(
            std::fs::read_to_string(
                new.home
                    .join(".claude/projects")
                    .join(claude_slug(&notes_new))
                    .join("memory/MEMORY.md")
            )
            .unwrap(),
            "notes-mem"
        );
        let cj: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(new.home.join(".claude.json")).unwrap())
                .unwrap();
        assert!(cj["projects"].get(&notes_new).is_some(), "{cj}");
        assert!(cj["projects"].get(&notes_old).is_none());
        assert_eq!(
            claude_slug("/home/livren/Kerja/api_mobile v2"),
            "-home-livren-Kerja-api-mobile-v2"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(new.home.join(".ssh/id_ed25519"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // only one section
        let part = Homes {
            home: dir.path().join("part-home"),
            config: dir.path().join("part-home/.config"),
            root: dir.path().join("part-root"),
        };
        Layout::new(&part.root).ensure().unwrap();
        let rep = restore(
            &part,
            &RestoreRequest {
                file,
                password: "correct horse".into(),
                sections: vec!["ssh".into()],
            },
            &|_| {},
        )
        .unwrap();
        assert_eq!(rep.restored, 2);
        assert!(!part.home.join(".gitconfig").exists());
        assert_eq!(
            rep.repos_to_clone.len(),
            1,
            "projects not restored -> clone list"
        );
    }
}
