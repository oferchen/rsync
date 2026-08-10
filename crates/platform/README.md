# platform

Platform-specific unsafe code isolation - encapsulates all OS FFI (libc, Win32)
behind safe public APIs so higher-level crates remain 100% safe Rust.

## Modules

- `daemonize` - process daemonization (fork, setsid, stdio redirection)
- `env` - environment variable manipulation with RAII restoration
- `error` - typed platform error variants for I/O failures
- `group` - system group membership lookups
- `name_resolution` - Windows account name to RID resolution
- `privilege` - chroot, uid/gid dropping, privilege management
- `secrets` - secrets file permission validation
- `signal` - signal handler registration and shared atomic flags
- `windows_service` - Windows Service Control Manager (SCM) integration

## Dependencies

- **Upstream (Unix):** `libc`, `nix` (safe POSIX wrappers), `signal-hook`
- **Upstream (Windows):** `windows` crate (Win32 Foundation, Security, Services, Console)
- **Downstream:** `daemon`, `core`, `cli`

## Platform Notes

- **Unix:** uses `nix` for safe wrappers where available (chroot, setuid, setgid,
  setsid, dup2). Falls back to raw `libc` for operations not covered by nix
  (e.g., `setgroups` on macOS, `fork`, `getgrnam_r`).
- **Windows:** uses the `windows` crate for Win32 API bindings. Unsafe blocks are
  scoped to individual functions with `// SAFETY:` annotations.
- This crate uses `#![deny(unsafe_code)]` at the crate level. Individual functions
  requiring unsafe are annotated with `#[allow(unsafe_code)]`.
