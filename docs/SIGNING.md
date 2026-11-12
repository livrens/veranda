# Code signing: why installers get "unknown publisher" warnings, and how to remove them

Veranda's installers are built on GitHub Actions from the public source, and every update is
verified against the project's own update key. That protects users from tampered downloads, but
operating systems judge *who* signed a program, not how it was built:

| Platform | What users see without a certificate | What removes it |
| --- | --- | --- |
| Windows | SmartScreen: "Windows protected your PC", "Unknown publisher"; some antivirus products flag new unsigned executables | An Authenticode code-signing certificate (OV or EV) or Azure Trusted Signing. SmartScreen reputation then builds within days; EV certificates get it immediately |
| macOS | Gatekeeper: "cannot be opened because the developer cannot be verified" | An Apple Developer Program membership (USD 99/year): a Developer ID Application certificate plus notarization |
| Linux | Nothing: .deb/.rpm/.AppImage are installed by the user's package tools | Not needed |

Nothing in Veranda's code triggers these warnings; any unsigned program built today sees them.
Antivirus "false positives" come from the same place: new, unsigned, self-updating binaries look
like what malware looks like. Signing is the only durable fix. Veranda helps on its side by
keeping everything on loopback, downloading only over HTTPS with checksums, never running as
administrator except for the two documented steps, and publishing reproducible builds from a
public repository (see `docs/SECURITY.md`).

## What to buy

- **Windows**: either a code-signing certificate from a CA (Sectigo, DigiCert, SSL.com, GlobalSign;
  OV ≈ USD 100–250/year, EV ≈ USD 250–500/year, EV ships on a hardware token or cloud HSM), or
  **Azure Trusted Signing** (≈ USD 10/month, needs an Azure account and, for individuals, identity
  validation). Azure Trusted Signing needs a different `signCommand` setup; ask when you get there.
- **macOS**: Apple Developer Program (USD 99/year). Create a *Developer ID Application*
  certificate in Xcode or on developer.apple.com, export it as a `.p12` with a password, and
  create an app-specific password for notarization at appleid.apple.com.

## GitHub secrets the workflow understands

The release workflow signs automatically when these repository secrets exist (Settings →
Secrets and variables → Actions). Without them it builds unsigned, as today.

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | The `.p12` file, base64-encoded (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | Password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID e-mail used for notarization |
| `APPLE_PASSWORD` | An app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | The 10-character team id |
| `WINDOWS_CERTIFICATE` | The `.pfx` file, base64-encoded (`base64 -w0 cert.pfx`) |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password of the `.pfx` |

EV certificates on hardware tokens cannot be exported to a `.pfx`; they need a self-hosted runner
with the token attached, or a cloud signing service.

## Keep these secrets secret

Never commit certificates, `.pfx`/`.p12` files or passwords. They live only in GitHub Secrets
(masked in logs). `.gitignore` already excludes `*.key`, `*.pfx` and `*.p12`. The updater key
pair (`TAURI_SIGNING_PRIVATE_KEY`) is a separate thing: it protects updates, not the first
install, and stays as it is.

## Until the installers are signed

- Windows: in the SmartScreen dialog choose **More info → Run anyway**.
- macOS: right-click the app → **Open**, or run `xattr -d com.apple.quarantine /Applications/Veranda.app`.
- If an antivirus product quarantines Veranda, report it as a false positive with the download
  URL and the SHA-256 from the release page; vendors usually whitelist within days.
