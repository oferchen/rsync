#![deny(unsafe_code)]

use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let status = rsync_daemon::run(env::args_os(), &mut stdout, &mut stderr);
    rsync_daemon::exit_code_from(status)
}
