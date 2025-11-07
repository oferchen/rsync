# oc-rsync-embedding

`oc-rsync-embedding` exposes programmatic entry points for the Rust `oc-rsync`
client and daemon. Applications can call into the same logic used by the CLI
binaries without spawning a child process, mirroring the library-first approach
adopted by gokrazy's rsync packages. Each helper returns the exact exit status
reported by the corresponding binary and provides access to captured standard
output and error streams so embedding applications can surface diagnostics or
render help text inline.

## Examples

Run the client entry point with `--version` and inspect the captured output:

```rust
use oc_rsync_embedding::run_client;
use oc_rsync_core::branding::client_program_name;

let output = run_client([client_program_name(), "--version"])
    .expect("--version succeeds");

let banner = String::from_utf8(output.stdout().to_vec()).expect("utf-8");
assert!(
    banner.starts_with(client_program_name()),
    "version banner should begin with the invoked program name"
);
```

Forward custom writers and detect non-zero exit statuses:

```rust
use oc_rsync_embedding::run_client_with;
use oc_rsync_embedding::ExitStatusError;
use oc_rsync_core::branding::client_program_name;

let mut stdout = Vec::new();
let mut stderr = Vec::new();
let status = run_client_with(
    [client_program_name(), "--definitely-invalid"],
    &mut stdout,
    &mut stderr,
);

match status {
    Ok(()) => panic!("unexpected success"),
    Err(error) => {
        assert_eq!(error.exit_status(), 1);
        assert!(stderr.starts_with(b"rsync error:"));
    }
}
```

Drive the daemon parser with CLI-style arguments:

```rust
use oc_rsync_core::branding::daemon_program_name;
use oc_rsync_embedding::run_daemon;

let output = run_daemon([daemon_program_name(), "--help"]).unwrap();
assert!(
    String::from_utf8_lossy(output.stdout()).contains("Usage:"),
    "help output should contain a usage summary"
);
```

The crate also re-exports `oc_rsync_daemon::DaemonConfig` and
`oc_rsync_daemon::run_daemon` so long-running daemons can reuse the builder API
without constructing a command-line argument list first.
