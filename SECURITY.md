# Security policy

Veranda is a local development tool, but it still handles downloads, certificates and a database.
See [docs/SECURITY.md](docs/SECURITY.md) for the threat model.

Supported versions: the latest minor release.

Reporting: open a private issue or contact the repository owner directly. Please do not publish
details before a fix is available.

Dependency hygiene: `cargo audit` and `cargo outdated` run in CI; dependencies are pinned to the
newest stable versions at release time.
