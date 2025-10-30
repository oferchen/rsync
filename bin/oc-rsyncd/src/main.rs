#![deny(unsafe_code)]

use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    rsync_windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let status = rsync_daemon::run(env::args_os(), &mut stdout, &mut stderr);
    rsync_daemon::exit_code_from(status)
}
