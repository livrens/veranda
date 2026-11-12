//! Text files for the built-in editor: list folders, read and save documents, create, rename
//! and delete. Binary files and anything over [`MAX_BYTES`] are refused, saves preserve the
//! file's permissions and refuse to overwrite a file that changed on disk since it was read.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Largest file the editor opens.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: u64,
    pub hidden: bool,
    /// A dependency / build folder (`node_modules`, `vendor`, `target`...).
    pub heavy: bool,
    /// Editor language id for files (see [`language_for`]).
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub path: String,
    pub name: String,
    pub text: String,
    pub size: u64,
    pub mtime: u64,
    pub crlf: bool,
    pub language: String,
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn check_abs(p: &Path) -> Result<()> {
    if !p.is_absolute() {
        bail!("path must be absolute");
    }
    Ok(())
}

/// Editor language from the file name. Unknown files are `text`.
pub fn language_for(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "dockerfile" | "containerfile" => return "dockerfile",
        "makefile" | "gnumakefile" => return "makefile",
        ".env" | ".env.example" | ".env.local" | ".env.production" | ".env.testing" => {
            return "env";
        }
        ".gitignore" | ".dockerignore" | ".editorconfig" | ".npmrc" | ".htaccess" => {
            return "ini";
        }
        "composer.lock" | "package-lock.json" | ".babelrc" | ".eslintrc" | ".prettierrc" => {
            return "json";
        }
        _ => {}
    }
    if name.starts_with(".env.") {
        return "env";
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" | "mdx" => "markdown",
        "json" | "jsonc" | "json5" | "webmanifest" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" | "cnf" | "properties" | "editorconfig" => "ini",
        "env" => "env",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" | "tsx" => "typescript",
        "php" | "phtml" | "blade" => "php",
        "py" | "pyi" => "python",
        "go" => "go",
        "rs" => "rust",
        "java" | "kt" | "kts" | "scala" => "java",
        "c" | "h" | "cpp" | "cc" | "hpp" | "cs" | "swift" | "m" => "c",
        "rb" => "ruby",
        "sql" => "sql",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "batch",
        "css" | "scss" | "sass" | "less" => "css",
        "html" | "htm" | "xml" | "xhtml" | "svg" | "vue" | "svelte" | "twig" | "xsl" | "plist"
        | "csproj" => "html",
        "csv" | "tsv" => "csv",
        "log" => "log",
        "txt" | "text" | "rst" | "adoc" | "lock" | "" => "text",
        _ => "text",
    }
}

/// Entries of a folder: directories first, then files, both case-insensitively sorted.
pub fn list(dir: &Path) -> Result<Vec<Entry>> {
    check_abs(dir)?;
    let rd = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    let mut out: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = std::fs::metadata(e.path()).ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            Some(Entry {
                hidden: name.starts_with('.'),
                heavy: meta.is_dir() && crate::transfer::HEAVY_DIRS.contains(&name.as_str()),
                language: if meta.is_dir() {
                    String::new()
                } else {
                    language_for(&p).to_string()
                },
                path: p.display().to_string(),
                dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: mtime_of(&meta),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    head.contains(&0)
}

/// Read a text file for editing.
pub fn read(path: &Path) -> Result<Document> {
    check_abs(path)?;
    let meta = std::fs::metadata(path).with_context(|| format!("opening {}", path.display()))?;
    if meta.is_dir() {
        bail!("{} is a folder", path.display());
    }
    if meta.len() > MAX_BYTES {
        bail!(
            "{} is {} MB; the editor opens files up to {} MB",
            path.display(),
            meta.len() / 1_048_576,
            MAX_BYTES / 1_048_576
        );
    }
    let bytes = std::fs::read(path)?;
    if looks_binary(&bytes) {
        bail!("{} is not a text file", path.display());
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    let crlf = text.contains("\r\n");
    Ok(Document {
        path: path.display().to_string(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        text: if crlf {
            text.replace("\r\n", "\n")
        } else {
            text
        },
        size: meta.len(),
        mtime: mtime_of(&meta),
        crlf,
        language: language_for(path).to_string(),
    })
}

/// Save `text` (LF inside; written with CRLF when `crlf`). When `expect_mtime` is given and
/// the file changed since then, nothing is written and an error says so. Returns the new mtime.
pub fn write(path: &Path, text: &str, crlf: bool, expect_mtime: Option<u64>) -> Result<u64> {
    check_abs(path)?;
    let existing = std::fs::metadata(path).ok();
    if let (Some(meta), Some(expected)) = (&existing, expect_mtime)
        && mtime_of(meta) != expected
        && expected != 0
    {
        bail!("the file changed on disk since you opened it; reload it first");
    }
    let data = if crlf {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    };
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, data.as_bytes())?;
    tmp.as_file().sync_all()?;
    if let Some(meta) = &existing {
        let _ = std::fs::set_permissions(tmp.path(), meta.permissions());
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(std::fs::metadata(path).map(|m| mtime_of(&m)).unwrap_or(0))
}

/// Create an empty file or a folder. Fails when something is already there.
pub fn create(path: &Path, dir: bool) -> Result<()> {
    check_abs(path)?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() || name.contains(['/', '\\']) {
        bail!("invalid name");
    }
    if dir {
        std::fs::create_dir_all(path)?;
    } else {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, b"")?;
    }
    Ok(())
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    check_abs(from)?;
    check_abs(to)?;
    if to.exists() {
        bail!("{} already exists", to.display());
    }
    std::fs::rename(from, to).with_context(|| format!("renaming {}", from.display()))
}

/// Delete a file or a whole folder.
pub fn delete(path: &Path) -> Result<()> {
    check_abs(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Folders the Files page offers first: the document root, the home, and the folders picked
/// for move bundles.
pub fn roots(layout: &crate::paths::Layout, cfg: &crate::config::Config) -> Vec<PathBuf> {
    let mut v = vec![cfg.document_root(layout)];
    for f in &cfg.transfer.folders {
        let p = PathBuf::from(f);
        if p.is_dir() && !v.contains(&p) {
            v.push(p);
        }
    }
    if let Some(h) = dirs::home_dir()
        && !v.contains(&h)
    {
        v.push(h);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(language_for(Path::new("/a/README.md")), "markdown");
        assert_eq!(language_for(Path::new("/a/.env.local")), "env");
        assert_eq!(language_for(Path::new("/a/Dockerfile")), "dockerfile");
        assert_eq!(language_for(Path::new("/a/composer.lock")), "json");
        assert_eq!(language_for(Path::new("/a/x.blade.php")), "php");
        assert_eq!(language_for(Path::new("/a/notes")), "text");
    }

    #[test]
    fn read_write_roundtrip_and_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("notes.md");
        std::fs::write(&f, "# hi\r\nline\r\n").unwrap();
        let d = read(&f).unwrap();
        assert!(d.crlf);
        assert_eq!(d.text, "# hi\nline\n");
        assert_eq!(d.language, "markdown");
        let m = write(&f, "# hi\nchanged\n", d.crlf, Some(d.mtime)).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"# hi\r\nchanged\r\n");
        // stale mtime -> refused
        std::fs::write(&f, "other").unwrap();
        let meta = std::fs::metadata(&f).unwrap();
        if mtime_of(&meta) != m {
            assert!(write(&f, "x", false, Some(m)).is_err());
        }
        // binary refused, folder listing sorted
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
        assert!(read(&dir.path().join("bin.dat")).is_err());
        create(&dir.path().join("sub"), true).unwrap();
        create(&dir.path().join("sub/a.txt"), false).unwrap();
        assert!(create(&dir.path().join("sub"), true).is_err());
        let l = list(dir.path()).unwrap();
        assert_eq!(l[0].name, "sub");
        assert!(l[0].dir);
        rename(&dir.path().join("sub/a.txt"), &dir.path().join("sub/b.txt")).unwrap();
        assert!(dir.path().join("sub/b.txt").is_file());
        delete(&dir.path().join("sub")).unwrap();
        assert!(!dir.path().join("sub").exists());
        assert!(read(Path::new("relative.txt")).is_err());
    }
}
