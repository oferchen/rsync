# cli

The `cli` crate exposes the command-line front-end used by the
`oc-rsync` binary. It provides argument parsing, help text rendering, and
version output that mirror upstream rsync 3.4.1 while delegating transfer
execution to the shared `core` facade.

## Features

- Deterministic `--help` and `--version` rendering with branded program
  names.
- Parsing for the subset of rsync options that the workspace currently
  implements, including archive mode, deletion controls, filter rules,
  bandwidth limiting, remote-shell configuration, and file list sources.
- Dispatch to `core` for data transfer, progress reporting, and
  diagnostic handling so behavior stays centralised.

## Optional capabilities

The crate inherits the `xattr` and `acl` feature flags from `core`.
Enabling them exposes the associated metadata preservation flags in the
command-line parser and ensures capability reporting includes the
compiled-in features.

## Examples

The crate is primarily consumed by the `oc-rsync` binary:

```rust,no_run
use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let status = cli::run(env::args_os(), &mut stdout, &mut stderr);
    cli::exit_code_from(status)
}
```

See the crate documentation for more examples covering option parsing and
error handling.
