# oc-rsync

[![CI](https://github.com/oferchen/oc-rsync/actions/workflows/ci.yml/badge.svg)](https://github.com/oferchen/oc-rsync/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/oferchen/oc-rsync)](https://github.com/oferchen/oc-rsync/releases)

oc-rsync is a pure-Rust reimplementation of the rsync protocol (version 32) that
tracks upstream rsync 3.4.1 while publishing the branded release string
`3.4.1-rust`. The workspace ships the canonical client/daemon binaries
`oc-rsync` and `oc-rsyncd`. The optional `legacy-binaries` feature (enabled for
packaged releases) additionally builds compatibility wrappers `rsync` and
`rsyncd`, letting existing deployments switch between the legacy and branded
entry points without changing behaviour.

The authoritative source repository is hosted at
<https://github.com/oferchen/rsync>.

## Workspace essentials

- `crates/core` — shared message, branding, and configuration primitives reused
  across every binary.
- `crates/cli` — argument parsing and orchestration for the client and daemon
  front-ends.
- `crates/engine` — deterministic local-copy executor that preserves metadata,
  sparse extents, and optional ACL/xattr state.
- `crates/protocol`, `crates/transport`, `crates/checksums`, `crates/walk`, and
  `crates/logging` — protocol negotiation, IO transports, checksum suites,
  filesystem traversal, and logging infrastructure.

## Running the project

```bash
cargo test
cargo run -p xtask -- docs
cargo run -p xtask -- release
cargo run -p xtask -- package
```

Inspect the canonical branding, version metadata, and configuration paths
without hard-coding values by invoking:

```bash
cargo run -p xtask -- branding --json
```

## Daemon configuration

The `oc-rsyncd` daemon loads its runtime configuration from
`/etc/oc-rsyncd/oc-rsyncd.conf`, with shared secret credentials stored
alongside it in `/etc/oc-rsyncd/oc-rsyncd.secrets`. Those paths are derived
from the workspace branding metadata and match the values reported by the
`cargo run -p xtask -- branding --json` helper, ensuring local development and
packaged artifacts stay aligned.

## License

This project is available under the GPL-3.0-or-later license. See `LICENSE` for
full terms.
