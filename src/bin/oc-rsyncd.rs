#![deny(unsafe_code)]

#[path = "client.rs"]
mod client;
#[path = "daemon_wrapper.rs"]
mod daemon_wrapper;
mod support;

use std::ffi::OsString;
use std::io::Write;
use std::{env, io, process::ExitCode};

use oc_rsync_core::branding::Brand;

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    oc_rsync_windows_gnu_eh::force_link();

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
    let fallback_program = Brand::Oc
        .daemon_wrapper_program_name()
        .unwrap_or_else(|| Brand::Oc.daemon_program_name());
    let forwarded = daemon_wrapper::wrap_daemon_arguments(args, fallback_program);
    client::run_with(forwarded, stdout, stderr)
}
