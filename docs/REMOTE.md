# Remote (SSH client)

The Remote group in the sidebar is an SSH client built into Veranda. It is implemented in the
`veranda-remote` crate and wired to the UI by `apps/desktop/src-tauri/src/remote.rs` and
`apps/desktop/ui/remote.js`.

## Where data lives

| What | Where | Notes |
| --- | --- | --- |
| Hosts, groups, identities, key metadata, snippets, rules, history, settings | `etc/remote/store.json` | one JSON file, user-only permissions, written atomically; snippet bodies are stored sealed (`script_enc`) once a snippet password is set |
| Snippet lock | `etc/remote/snippet-lock.json` | salt, PBKDF2 round count, verifier and auto-lock minutes; the key is never written |
| Private keys | `etc/remote/keys/<id>.key` | OpenSSH format, optionally passphrase-encrypted, mode 0600 |
| Accepted server keys | `etc/remote/known_hosts` | OpenSSH `known_hosts` format |
| Passwords and passphrases | OS credential store (service "Veranda") | macOS Keychain, Windows Credential Manager, Secret Service / KWallet |
| Fallback for secrets | `secrets/remote-secrets.json` | only when no credential store answers; user-only permissions; the Keychain page says which backend is active |

Nothing is synced anywhere, and Veranda never reads the data of other SSH clients. "Import"
only parses `~/.ssh/config` (and PuTTY sessions from the Windows registry) when you click it.

## Connecting

1. The host (and its jump host, recursively) is resolved into a connect request: target,
   username, and an ordered list of methods: saved key / password / agent according to the
   host's *Auth*, then "ask for a password", then keyboard-interactive.
2. The server key is checked against `known_hosts`. Unknown keys are shown with their SHA-256
   fingerprint; a changed key is flagged as such and must be confirmed explicitly.
3. Each method is tried in order; the user is prompted (password, passphrase, one-time code)
   through the `remote-prompt` event. "Remember" stores the answer in the secret store after a
   successful login.
4. A terminal opens a `session` channel with a `xterm-256color` PTY; SFTP opens the `sftp`
   subsystem; forwarding rules open `direct-tcpip` channels (local, SOCKS5) or ask the server to
   `tcpip-forward` (remote). Every tab, SFTP session and rule has its own SSH connection.

Keep-alive packets are sent at the interval in the terminal settings (default 30 s). The local
terminal is the user's shell (`$SHELL`, PowerShell on Windows) with Veranda's runtimes on `PATH`.

## Crypto

Pure Rust: `russh` with the `ring` backend (the same crypto that `rustls` uses elsewhere in
Veranda). Key types: Ed25519, ECDSA P-256/384/521, RSA (rsa-sha2-256/512 negotiated through
`server-sig-algs`). Generated keys: Ed25519 by default, RSA 4096, ECDSA P-256. Private keys are
encrypted with the OpenSSH `aes256-ctr` + `bcrypt` scheme when a passphrase is set.

Snippet lock (`vault.rs`): PBKDF2-HMAC-SHA256 with 600 000 rounds and a 16-byte random salt
derives 64 bytes from the password; bytes 0-31 are the AES-256-GCM key, bytes 32-63 the verifier
kept on disk. Each snippet body is sealed with a fresh 12-byte nonce (`v1:` + base64 of nonce,
ciphertext and tag). The key lives only in the desktop process while unlocked, is wiped when
dropped, and is forgotten on **Lock**, after the chosen idle time, or when Veranda exits. Wrong
passwords add a growing delay (up to 3 s) before the next attempt. The UI never receives the
ciphertext: `remote_state` sends the plain text when unlocked and an empty body otherwise, and
`term_send_snippet` types a snippet into a session server-side so the panel only needs names.

## Tests

`cargo test -p veranda-remote` runs unit tests for the store, keys, known hosts, SSH config
parsing and SOCKS parsing, plus an end-to-end test against an in-process `russh` server
(password and key login, exec, shell, local and dynamic forwards, refused host keys).
