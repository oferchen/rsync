# Windows Daemon Full Functionality Design

Date: 2026-03-29

## Problem

The daemon crate compiles on Windows but most functionality is stubbed out as no-ops.
Daemonization, signal handling, privilege dropping, name resolution, group expansion,
and logging all do nothing on Windows. Users cannot run a production daemon on Windows.

## Decisions

| Feature | Approach |
|---------|----------|
| Daemonization | Dual-mode: console (default) + Windows Service via `--windows-service` |
| Signals | `SetConsoleCtrlHandler` (console) / SCM control handler (service) |
| Privilege dropping | Token impersonation via `LogonUserW` + `ImpersonateLoggedOnUser` |
| Chroot | Skip with warning (no Windows equivalent) |
| Name resolution | Direct API calls: `LookupAccountSidW` / `LookupAccountNameW` |
| Group expansion | Local groups via `NetLocalGroupGetMembers` (no AD/LDAP) |
| Logging | stderr (console mode) / Windows Event Log (service mode) |

## Architecture

Single new dependency: `windows` crate with targeted feature flags. No wrapper crates.

One new file: `windows_service.rs`. Everything else extends existing files with
`#[cfg(windows)]` blocks alongside the existing `#[cfg(unix)]` implementations.

```
crates/daemon/src/daemon/sections/
  daemonize.rs          -- #[cfg(windows)]: service registration OR no-op console
  signals.rs            -- #[cfg(windows)]: SetConsoleCtrlHandler + named event reload
  privilege.rs          -- #[cfg(windows)]: LogonUserW + ImpersonateLoggedOnUser
  name_converter.rs     -- #[cfg(windows)]: LookupAccountSidW / LookupAccountNameW
  group_expansion.rs    -- #[cfg(windows)]: NetLocalGroupGetMembers
  server_runtime/
    accept_loop.rs      -- #[cfg(windows)]: Event Log (service) or stderr (console)
crates/daemon/src/
  systemd.rs            -- unchanged (already no-op on non-Linux)
  windows_service.rs    -- NEW: service dispatch, control handler, status reporting
```

## Component 1: Windows Service Module (`windows_service.rs`)

Handles the SCM lifecycle when `--windows-service` is passed.

**Service lifecycle:**

1. `service_main()` - Entry point called by SCM. Registers the control handler,
   reports `SERVICE_START_PENDING`, then calls the normal `run_accept_loop()`.
2. `service_control_handler()` - Receives SCM events and maps to `SignalFlags`:
   - `SERVICE_CONTROL_STOP` / `SERVICE_CONTROL_SHUTDOWN` -> `shutdown` flag (= SIGTERM)
   - `SERVICE_CONTROL_PARAMCHANGE` -> `reload_config` flag (= SIGHUP)
   - `SERVICE_CONTROL_PRESHUTDOWN` -> `graceful_exit` flag (= SIGUSR1)
3. Status reporting mirrors `systemd.rs` interface:
   - `report_running()` -> `SetServiceStatus(SERVICE_RUNNING)`
   - `report_stopping()` -> `SetServiceStatus(SERVICE_STOP_PENDING)`

**Console mode** (default, no `--windows-service`): Runs the accept loop in the
foreground like `--no-detach` on Unix. `SetConsoleCtrlHandler` handles Ctrl+C/Ctrl+Break.

**Installation helpers:**
- `oc-rsync --daemon --install-service` registers the service via `CreateServiceW()`
  with `--windows-service` in the command line.
- `oc-rsync --daemon --uninstall-service` removes it via `DeleteService()`.
- Users can also use `sc.exe create` directly.

The `SignalFlags` struct is shared between Unix and Windows - both platforms set the
same atomic flags through different mechanisms.

## Component 2: Signal Handling (`signals.rs`)

Replace the current no-op stub:

- **Console mode:** `SetConsoleCtrlHandler` callback:
  - `CTRL_C_EVENT` -> shutdown flag
  - `CTRL_BREAK_EVENT` -> graceful exit flag
- **Service mode:** Signals come from SCM control handler (Component 1).
- **Config reload:** Create named event `Global\oc-rsync-reload-{pid}`. External
  tools signal it to trigger config reload (= `kill -HUP`). Background thread
  waits with `WaitForSingleObject`.

## Component 3: Privilege Dropping (`privilege.rs`)

Replace no-op stubs:

- `drop_privileges(uid, gid)` - The `uid` config directive accepts a Windows
  username (e.g., `uid = NetworkService` or `uid = DOMAIN\user`). Calls
  `LogonUserW()` to obtain a token, then `ImpersonateLoggedOnUser()` on the
  connection handler thread. Reverts on thread exit.
- `apply_chroot()` - Logs warning: "chroot is not supported on Windows, ignoring
  `use chroot` directive". Returns `Ok(())`.

## Component 4: Name Resolution (`name_converter.rs`)

Add `#[cfg(windows)]` implementation:

- `WindowsNameConverter` struct - direct API calls, no subprocess
- `sid_to_name()` via `LookupAccountSidW()`
- `name_to_sid()` via `LookupAccountNameW()`
- Implements the same `NameConverterCallbacks` trait as the Unix version

## Component 5: Group Expansion (`group_expansion.rs`)

Replace `Ok(None)` stub:

- `lookup_group_members()` calls `NetLocalGroupGetMembers()` at level 3
  (returns `LOCALGROUP_MEMBERS_INFO_3` with domain-qualified names)
- Free buffer with `NetApiBufferFree()`
- Returns `Vec<String>` of member names

## Component 6: Logging (`accept_loop.rs`)

Replace syslog stub with dual-mode:

- **Console mode:** stderr via existing `eprintln!` paths. No changes needed.
- **Service mode:** Register event source with `RegisterEventSourceW("oc-rsync")`.
  Map levels: error -> `EVENTLOG_ERROR_TYPE`, warning -> `EVENTLOG_WARNING_TYPE`,
  info -> `EVENTLOG_INFORMATION_TYPE`. Write via `ReportEventW()`. Deregister
  with `DeregisterEventSource()` on shutdown.

The `syslog_facility` config directive is ignored on Windows with a debug log.

## Dependencies

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Security_Authentication_Identity",
    "Win32_System_Services",
    "Win32_System_Console",
    "Win32_System_EventLog",
    "Win32_NetworkManagement_NetManagement",
] }
```

## CLI Additions

Three Windows-only flags on `--daemon` (gated with `#[cfg(windows)]`):

- `--windows-service` - run as SCM service
- `--install-service` - register the service and exit
- `--uninstall-service` - remove the service and exit

## Testing Strategy

**Unit tests** (per module, `#[cfg(test)]` + `#[cfg(windows)]`):

| Module | Test |
|--------|------|
| signals | `SetConsoleCtrlHandler` registration, flag atomics |
| privilege | `LogonUserW` with current user, impersonation round-trip |
| group_expansion | `NetLocalGroupGetMembers` for built-in "Users" group |
| name_converter | `LookupAccountNameW("SYSTEM")` SID round-trip |

**Integration tests:**
- Console daemon: start, accept TCP, serve module, Ctrl+C shutdown
- Service install/uninstall round-trip (admin only, skip in CI)
- Config reload via named event
- `uid = NetworkService` impersonation on connection handler thread
- Chroot directive logs warning but does not fail

**CI:** Existing Windows matrix runs new tests automatically. Service
install/uninstall gated behind admin privilege check - skip gracefully in CI.

## Phased Delivery

| Phase | Scope | PR |
|-------|-------|----|
| 1 | Console mode signals + logging | Smallest useful increment |
| 2 | Privilege dropping + name resolution + group expansion | Security features |
| 3 | Windows Service (SCM) + install/uninstall helpers | Production-ready |

Each phase is a separate PR, independently testable.
