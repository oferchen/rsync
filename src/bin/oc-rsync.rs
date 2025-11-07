#![deny(unsafe_code)]

#[path = "client.rs"]
mod client;
mod support;

use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    oc_rsync_windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    client::run_with(env::args_os(), &mut stdout, &mut stderr)
}
