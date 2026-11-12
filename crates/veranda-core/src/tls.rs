//! Local certificate authority and site certificates (ECDSA P-256, TLS 1.2/1.3 only).
//!
//! * `etc/ssl/ca.crt` / `ca.key` – the CA (key is 0600, never leaves this machine)
//! * `etc/ssl/sites.crt` / `sites.key` – one leaf cert whose SANs cover every site
//!
//! Trusting the CA is opt-in and uses the *current user's* store where possible.

use crate::fsutil;
use crate::paths::Layout;
use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use std::path::PathBuf;

pub const CA_NAME: &str = "Veranda Local Development CA";

pub struct Paths {
    pub ca_crt: PathBuf,
    pub ca_key: PathBuf,
    pub sites_crt: PathBuf,
    pub sites_key: PathBuf,
}

pub fn paths(layout: &Layout) -> Paths {
    let d = layout.ssl();
    Paths {
        ca_crt: d.join("ca.crt"),
        ca_key: d.join("ca.key"),
        sites_crt: d.join("sites.crt"),
        sites_key: d.join("sites.key"),
    }
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, CA_NAME);
    dn.push(DnType::OrganizationName, "Veranda");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365 * 10);
    params
}

/// Create the CA if missing. Returns true when a new CA was generated.
pub fn ensure_ca(layout: &Layout) -> Result<bool> {
    let p = paths(layout);
    // The CA's name is recorded next to it; a CA from before the rename (or with another name)
    // is replaced so the browser shows the current product name in its trust list.
    let stamp = layout.ssl().join("ca.name");
    let current = std::fs::read_to_string(&stamp).unwrap_or_default();
    if p.ca_crt.is_file() && p.ca_key.is_file() && current.trim() == CA_NAME {
        return Ok(false);
    }
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let cert = ca_params().self_signed(&key)?;
    fsutil::write_private(&p.ca_key, key.serialize_pem().as_bytes())?;
    fsutil::write_atomic(&p.ca_crt, cert.pem().as_bytes())?;
    fsutil::write_atomic(&stamp, CA_NAME.as_bytes())?;
    // A new CA invalidates old leaf certs.
    let _ = std::fs::remove_file(&p.sites_crt);
    let _ = std::fs::remove_file(&p.sites_key);
    let _ = std::fs::remove_file(layout.ssl().join("sites.sans"));
    Ok(true)
}

fn load_ca_key(layout: &Layout) -> Result<KeyPair> {
    let pem = std::fs::read_to_string(paths(layout).ca_key).context("reading CA key")?;
    Ok(KeyPair::from_pem(&pem)?)
}

/// (Re)issue the site certificate for `hosts` (DNS names; IPs allowed). Always includes
/// localhost and 127.0.0.1. Returns true when the file changed.
pub fn ensure_site_cert(layout: &Layout, hosts: &[String]) -> Result<bool> {
    ensure_ca(layout)?;
    let p = paths(layout);
    let mut sans: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    for h in hosts {
        let h = h.trim().to_ascii_lowercase();
        if h.is_empty() || sans.contains(&h) {
            continue;
        }
        sans.push(h.clone());
        if !h.starts_with("*.") && h.contains('.') && h.parse::<std::net::IpAddr>().is_err() {
            sans.push(format!("*.{h}"));
        }
    }
    sans.sort();
    sans.dedup();

    // Skip re-issuing when the SAN set is unchanged and the cert is not close to expiry.
    let stamp_file = layout.ssl().join("sites.sans");
    let stamp = sans.join("\n");
    if p.sites_crt.is_file()
        && p.sites_key.is_file()
        && let Ok(existing) = std::fs::read_to_string(&stamp_file)
        && existing == stamp
        && !older_than_days(&p.sites_crt, 700)
    {
        return Ok(false);
    }

    let ca_key = load_ca_key(layout)?;
    let issuer = Issuer::new(ca_params(), ca_key);
    let mut params = CertificateParams::new(sans.clone())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "localhost");
    dn.push(DnType::OrganizationName, "Veranda");
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(820); // < 825 days (Apple / Chrome policy)
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let cert = params.signed_by(&key, &issuer)?;
    fsutil::write_private(&p.sites_key, key.serialize_pem().as_bytes())?;
    fsutil::write_atomic(&p.sites_crt, cert.pem().as_bytes())?;
    fsutil::write_atomic(&stamp_file, stamp.as_bytes())?;
    Ok(true)
}

fn older_than_days(path: &std::path::Path, days: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.elapsed()
                .map(|e| e.as_secs() > days * 86_400)
                .unwrap_or(false)
        })
        .unwrap_or(true)
}

/// Commands that import the CA into trust stores. Each is a (description, program, args,
/// needs_admin). The caller decides how to run them (CLI prints, GUI runs with elevation).
pub fn trust_commands(layout: &Layout) -> Vec<TrustStep> {
    let ca = paths(layout).ca_crt;
    let ca_s = ca.to_string_lossy().into_owned();
    let mut v = Vec::new();
    if cfg!(target_os = "windows") {
        v.push(TrustStep {
            description:
                "Add to the current user's Trusted Root store (Windows prompts for confirmation)"
                    .into(),
            program: "certutil".into(),
            args: vec!["-user".into(), "-addstore".into(), "Root".into(), ca_s],
            needs_admin: false,
        });
    } else if cfg!(target_os = "macos") {
        let login = dirs::home_dir()
            .unwrap_or_default()
            .join("Library/Keychains/login.keychain-db");
        v.push(TrustStep {
            description: "Add to the login keychain as trusted root".into(),
            program: "security".into(),
            args: vec![
                "add-trusted-cert".into(),
                "-r".into(),
                "trustRoot".into(),
                "-k".into(),
                login.to_string_lossy().into_owned(),
                ca_s,
            ],
            needs_admin: false,
        });
    } else {
        // NSS stores – per user, no root needed. Chrome/Chromium/Brave share `~/.pki/nssdb`;
        // every Firefox profile has its own. The database is created when missing and an older
        // entry with our name (a previous CA) is replaced; certutil comes from libnss3-tools.
        let home = dirs::home_dir().unwrap_or_default();
        let mut stores: Vec<(String, std::path::PathBuf)> =
            vec![("Chrome, Chromium, Brave".into(), home.join(".pki/nssdb"))];
        for base in [
            home.join(".mozilla/firefox"),
            home.join("snap/firefox/common/.mozilla/firefox"),
            home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        ] {
            if let Ok(rd) = std::fs::read_dir(&base) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.join("cert9.db").is_file() || p.join("prefs.js").is_file() {
                        let name = p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        stores.push((format!("Firefox profile {name}"), p));
                    }
                }
            }
        }
        for (label, dir) in stores {
            let d = format!("sql:{}", dir.display());
            let (dq, cq, nq) = (sh_quote(&d), sh_quote(&ca_s), sh_quote(CA_NAME));
            let dirq = sh_quote(&dir.to_string_lossy());
            v.push(TrustStep {
                description: format!("Add to the NSS database ({label})"),
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    format!(
                        "command -v certutil >/dev/null || {{ echo 'certutil is missing: install libnss3-tools (Debian/Ubuntu) or nss-tools (Fedora)' >&2; exit 1; }}; \
                         mkdir -p {dirq}; [ -f {dirq}/cert9.db ] || certutil -d {dq} -N --empty-password; \
                         certutil -d {dq} -D -n {nq} >/dev/null 2>&1; certutil -d {dq} -A -t 'C,,' -n {nq} -i {cq}"
                    ),
                ],
                needs_admin: false,
            });
        }
        // System store (curl, php, wget) – needs root; path depends on the distro family.
        if std::path::Path::new("/usr/local/share/ca-certificates").is_dir() {
            v.push(TrustStep {
                description: "Install into the system store (Debian/Ubuntu)".into(),
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    format!("install -m 0644 '{ca_s}' /usr/local/share/ca-certificates/veranda-local-ca.crt && update-ca-certificates"),
                ],
                needs_admin: true,
            });
        } else if std::path::Path::new("/etc/pki/ca-trust/source/anchors").is_dir() {
            v.push(TrustStep {
                description: "Install into the system store (Fedora/RHEL)".into(),
                program: "sh".into(),
                args: vec!["-c".into(), format!("install -m 0644 '{ca_s}' /etc/pki/ca-trust/source/anchors/veranda-local-ca.crt && update-ca-trust")],
                needs_admin: true,
            });
        }
    }
    v
}

/// Single-quote a string for `sh -c`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Debug, Clone)]
pub struct TrustStep {
    pub description: String,
    pub program: String,
    pub args: Vec<String>,
    pub needs_admin: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_and_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.ensure().unwrap();
        assert!(ensure_ca(&layout).unwrap());
        assert!(!ensure_ca(&layout).unwrap());
        assert!(ensure_site_cert(&layout, &["app.test".into()]).unwrap());
        assert!(!ensure_site_cert(&layout, &["app.test".into()]).unwrap());
        assert!(ensure_site_cert(&layout, &["app.test".into(), "blog.test".into()]).unwrap());
        let pem = std::fs::read_to_string(paths(&layout).sites_crt).unwrap();
        assert!(pem.contains("BEGIN CERTIFICATE"));
        let sans = std::fs::read_to_string(layout.ssl().join("sites.sans")).unwrap();
        assert!(sans.contains("*.blog.test"));
    }
}
