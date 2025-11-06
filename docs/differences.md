# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. The canonical entrypoint ships under
the branded name **oc-rsync 3.4.1-rust** as declared in `Cargo.toml`, with an
optional shell wrapper installed as **oc-rsyncd** that forwards directly to
`oc-rsync --daemon`. Compatibility wrappers (**rsync**, **rsyncd**) share the
same execution paths for environments that still depend on the legacy
branding. Items remain in this list until the referenced functionality ships
and parity is verified by tests or goldens.

## Blocking Differences

- **`oc-rsync` client uses native local copies and a fallback for remote transfers**
  - *Impact*: `oc-rsync` performs deterministic local filesystem copies for
    regular files, directory trees, symbolic links, hard links, block/character
    devices, FIFOs, and sparse files while preserving permissions, timestamps,
    optional ownership metadata, and—when the default `acl`/`xattr` features are enabled—POSIX ACLs and extended
    attributes (these toggles remain configurable for custom builds). It can proactively create parent directories via `--mkpath`
    even when implied directories are disabled. Append-only updates via
    `--append` and `--append-verify` reuse the native verification logic when the
    destination already contains a prefix of the incoming file, and
    reference-directory modes (`--compare-dest`, `--copy-dest`, `--link-dest`)
    reuse the engine's delta detection to avoid unnecessary rewrites. A
    `--dry-run` flag validates transfers without mutating the destination, while
    `--delete`, `--delete-after`, `--delete-delay`, and `--delete-excluded`
    remove destination entries that are absent from the source. The client can
    contact an `rsync://` daemon to list available modules and, when remote
    operands are supplied, spawns the system `rsync` binary (configurable via
    `OC_RSYNC_FALLBACK`) so full network functionality remains available while
    the native transport and delta engine are built. The `rsync`
    compatibility wrapper reuses the same execution path. Filter handling via
    `--exclude`/`--exclude-from`/`--include`/`--include-from` and `--filter`
    with `+`/`-` actions, `show`/`hide`, `protect`/`risk`,
    `exclude-if-present=FILE`, and `merge`/`dir-merge` directives (including
    their `.`/`:` shorthands) mirrors rsync's glob semantics for local copies,
    though the more obscure filter modifiers are still pending. Progress
    reporting emits streaming, carriage-return updates akin to upstream `rsync`,
    and the `--stats` flag reports the engine's full set of counters
    (bytes sent/received, matched bytes, compression usage, file-list timings,
    and per-kind tallies). Delta-transfer framing and the remaining filter
    constructs are still outstanding for remote interoperability.
  - *Removal plan*: Implement the delta-transfer engine plus supporting crates,
    extend `core::client::run_client` to orchestrate protocol negotiation and
    comprehensive metadata handling, remove the fallback dependency, and
    validate the resulting behaviour via the parity harness.
- **Daemon functionality incomplete (`oc-rsync --daemon`)**
  - *Impact*: Invoking `oc-rsync --daemon` binds a TCP listener, performs the legacy
    `@RSYNCD:` handshake, and lists modules defined via `--module` arguments or
    a subset of `rsyncd.conf` supplied through `--config`. When the upstream
    `rsync` binary is available, the daemon now delegates authenticated module
    sessions to it by default so clients retain end-to-end transfers while the
    native data path is completed. Delegation can be disabled explicitly via
    `OC_RSYNC_DAEMON_FALLBACK=0`/`false` (or the shared `OC_RSYNC_FALLBACK`
    override); when disabled or when the helper binary is missing the daemon
    explains that transfers are unavailable after completing authentication.
    The `oc-rsyncd` and `rsyncd` compatibility wrappers expose the same behaviour
    through the branded and legacy binary names.
    Authentication and authorization flows are in place, and module-level
    `use chroot` directives are parsed with absolute-path enforcement, but real
    module serving and the broader directive matrix remain unimplemented when
    delegation is not possible.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief so negotiated
    sessions can progress beyond the initial diagnostic.
- **Transfer engine integration incomplete**
  - *Impact*: The `rsync_engine` crate provides deterministic local copies for
    regular files, directories, symbolic links, device nodes, FIFOs, extended
    attributes, and (when enabled) POSIX ACLs. Delta-token generation and
    application are available via
    [`DeltaGenerator`](../crates/engine/src/delta/generator.rs) and
    [`apply_delta`](../crates/engine/src/delta/script.rs), and local copies honour
    compression toggles for bandwidth limiting and statistics through
    [`CountingZlibEncoder`](../crates/engine/src/local_copy.rs). The client still
    spawns the system `rsync` for network transfers because the delta pipeline
    has not yet been wired into the protocol and transport layers, and the
    broader filter grammar remains a work in progress.
  - *Removal plan*: Wire the delta pipeline into the client/daemon transfer
    flow, finish the remaining filter semantics, and validate the combined
    behaviour via the parity harness before dropping the fallback dependency.
- **Parity harness and packaging verification still evolving**
  - *Impact*: CI now generates `.deb`, `.rpm`, and tarball artifacts for Linux
    (x86_64/aarch64), macOS (x86_64/aarch64), and Windows (x86_64/aarch64)
    using the `dist` profile with the Windows aarch64 lane currently disabled
    while the toolchain stabilises. Packaging installs `oc-rsync` under
    `/usr/bin` together with the shell wrapper `/usr/sbin/oc-rsyncd` so upstream
    rsync packages can remain in place, and
    optional alternatives registration stays disabled unless explicitly
    requested. The `tools/ci/run_interop.sh` harness downloads upstream
    releases 3.0.9, 3.1.3, and 3.4.1 and exercises both directions—upstream
    client to oc-rsyncd and oc-rsync to upstream daemons—to verify
    interoperability. Automated installation tests, golden filesystem
    comparisons, and the exit-code oracle have not landed yet, so production
    scope gating still depends on manual review for those aspects.
  - *Removal plan*: Add installation verification, parity goldens, and the
    exit-code oracle to CI so packaging and interop coverage become
    self-verifying.

All remaining behaviour currently matches the limited scope implemented in the
`protocol` crate; additional differences will be documented here as they are
observed.
