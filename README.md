# oc-rsync

oc-rsync is a pure-Rust reimplementation of the rsync protocol (version 32) that
tracks upstream rsync 3.4.1 while publishing the branded release string
`3.4.1-rust`. The workspace ships the canonical client/daemon binaries
`oc-rsync` and `oc-rsyncd` together with compatibility wrappers `rsync` and
`rsyncd`, so existing deployments can switch between the legacy and branded
entry points without changing behaviour.

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

## License

This project is available under the GPL-3.0-or-later license. See `LICENSE` for
full terms.
