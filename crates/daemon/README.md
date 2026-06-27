# daemon

Rsync daemon mode (`--daemon`) - TCP listener, protocol negotiation, and
module-based file serving.

## Purpose

`daemon` implements the rsync daemon process model: binding a TCP listener,
performing `@RSYNCD:` greeting negotiation (protocol 32), authenticating clients
via challenge/response, and executing transfers natively using the Rust transfer
engine. It handles per-module access control, chroot, uid/gid mapping, and
configuration via `oc-rsyncd.conf`.

## Key Public Types

- `run` / `run_daemon` - top-level entry points mirroring upstream `rsyncd`
- `DaemonConfig` / `DaemonConfigBuilder` - fluent configuration assembly
- `RsyncdConfig` - parsed `oc-rsyncd.conf` representation
- `ModuleConfig` - per-module settings (path, auth, read-only, filters)
- Authentication - SHA-512/256/SHA-1/MD5/MD4 challenge/response

## Dependencies (upstream)

`core`, `protocol`, `metadata`, `compress`, `checksums`, `fast_io`, `platform`,
`logging-sink`

## Dependents (downstream)

`cli` (daemon subcommand), `embedding`

## Features

- `sd-notify` - systemd readiness notification (Linux)
- `async-daemon` - tokio accept loop with sync worker dispatch
- `concurrent-sessions` - DashMap-backed session tracking
- `landlock` - Landlock LSM defense-in-depth (Linux 5.13+)

## Platform Notes

- Linux: Landlock sandboxing, systemd integration, io_uring via `fast_io`
- Windows: Win32 service primitives via `windows` crate
