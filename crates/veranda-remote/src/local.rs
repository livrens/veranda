//! The local shell used by "Local terminal" tabs.

use std::path::PathBuf;

/// Program + arguments of the user's shell. `preferred` (from settings) wins when it exists.
pub fn default_shell(preferred: &str) -> (String, Vec<String>) {
    let preferred = preferred.trim();
    if !preferred.is_empty() {
        let mut parts = preferred.split_whitespace();
        if let Some(p) = parts.next()
            && (PathBuf::from(p).exists() || which::which(p).is_ok())
        {
            return (p.to_string(), parts.map(String::from).collect());
        }
    }
    #[cfg(windows)]
    {
        for cand in ["pwsh.exe", "powershell.exe"] {
            if which::which(cand).is_ok() {
                return (cand.into(), vec!["-NoLogo".into()]);
            }
        }
        (
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()),
            vec![],
        )
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| PathBuf::from(s).exists())
            .or_else(|| {
                ["/bin/zsh", "/bin/bash", "/bin/sh"]
                    .iter()
                    .find(|p| PathBuf::from(p).exists())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "/bin/sh".into());
        let args = if shell.ends_with("bash") || shell.ends_with("zsh") || shell.ends_with("fish") {
            vec!["-l".to_string()]
        } else {
            vec![]
        };
        (shell, args)
    }
}
