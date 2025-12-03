# embedding

`embedding` exposes programmatic entry points for the Rust `oc-rsync`
client and daemon. Applications can call into the same logic used by the CLI
binaries without spawning a child process, mirroring the library-first approach
adopted by gokrazy's rsync packages. Each helper returns the exact exit status
reported by the corresponding binary and provides access to captured standard
output and error streams so embedding applications can surface diagnostics or
render help text inline.

## Examples

Run the client entry point with `--version` and inspect the captured output:

```no_run
use embedding::run_client;
use core::branding::client_program_name;

let output = run_client([client_program_name(), "--version"])
    .expect("--version succeeds");

let banner = String::from_utf8(output.stdout().to_vec()).expect("utf-8");
assert!(
    banner.starts_with(client_program_name()),
    "version banner should begin with the invoked program name"
);
```

Forward custom writers and detect non-zero exit statuses:

```no_run
use embedding::run_client_with;
use embedding::ExitStatusError;
use core::branding::client_program_name;

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

```no_run
use core::branding::daemon_program_name;
use embedding::run_daemon;

let output = run_daemon([daemon_program_name(), "--help"]).unwrap();
assert!(
    String::from_utf8_lossy(output.stdout()).contains("Usage:"),
    "help output should contain a usage summary"
);
```

The crate also re-exports `daemon::DaemonConfig` and
`daemon::run_daemon` so long-running daemons can reuse the builder API
without constructing a command-line argument list first.

## Server Mode

The embedding crate exposes server mode functionality for applications that need
to run the rsync server protocol programmatically. This is useful for:

- In-process testing of rsync protocol implementations
- Custom rsync server implementations
- Library usage scenarios where spawning a subprocess is not desirable

The server API provides direct access to the native server implementation without
CLI argument parsing:

```no_run
use embedding::{ServerConfig, ServerRole, run_server_with_config};
use std::io;

// Build server configuration from flag string and arguments
let config = ServerConfig::from_flag_string_and_args(
    ServerRole::Receiver,
    "-logDtpre.iLsfxC".to_string(),
    vec![".".into()],
).expect("valid server config");

// Run server with stdio (typically connected to SSH or other transport)
let mut stdin = io::stdin();
let mut stdout = io::stdout();

let stats = run_server_with_config(config, &mut stdin, &mut stdout)
    .expect("server execution succeeds");

// Inspect transfer statistics
match stats {
    embedding::ServerStats::Receiver(transfer_stats) => {
        println!("Received {} bytes", transfer_stats.bytes_received);
        println!("Files transferred: {}", transfer_stats.files_transferred);
    }
    embedding::ServerStats::Generator(generator_stats) => {
        println!("Sent {} bytes", generator_stats.bytes_sent);
        println!("Files transferred: {}", generator_stats.files_transferred);
    }
}
```

The crate re-exports `core::server::ServerConfig`, `ServerRole`, `ServerStats`,
and related types for convenient server embedding.
