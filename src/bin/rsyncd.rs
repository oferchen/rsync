#![deny(unsafe_code)]

#[path = "daemon.rs"]
mod daemon;
mod support;

use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    rsync_windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    daemon::run_with(env::args_os(), &mut stdout, &mut stderr)
}
