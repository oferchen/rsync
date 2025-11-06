#![deny(unsafe_code)]

#[path = "client.rs"]
mod client;
#[path = "daemon_wrapper.rs"]
mod daemon_wrapper;
mod support;

use std::ffi::OsString;
use std::io::Write;
use std::{env, io, process::ExitCode};

use rsync_core::branding::Brand;

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    rsync_windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_wrapper(env::args_os(), &mut stdout, &mut stderr)
}

fn run_wrapper<I, S, Out, Err>(args: I, stdout: &mut Out, stderr: &mut Err) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let forwarded = daemon_wrapper::wrap_daemon_arguments(args, Brand::Oc.daemon_program_name());
    client::run_with(forwarded, stdout, stderr)
}
