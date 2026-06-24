use std::ffi::OsString;
use std::io::Write;

use core::client::{
    AddressMode, BindAddress, ModuleListOptions, ModuleListRequest, TcpFastOpenMode,
    TransferTimeout, run_module_list_with_password_and_options,
};
use logging_sink::MessageSink;
use protocol::ProtocolVersion;
use rsync_io::ssh;

use crate::frontend::{execution::render_module_list, write_message};

/// Inputs for attempting a daemon module listing request.
pub(super) struct ModuleListingInputs<'a> {
    pub file_list_operands: &'a [OsString],
    pub remainder: &'a [OsString],
    pub daemon_port: Option<u16>,
    pub desired_protocol: Option<ProtocolVersion>,
    pub password_override: Option<Vec<u8>>,
    pub no_motd: bool,
    pub address_mode: AddressMode,
    pub bind_address: Option<&'a BindAddress>,
    pub connect_program: Option<&'a OsString>,
    pub remote_shell: Option<&'a OsString>,
    pub rsync_path: Option<&'a OsString>,
    pub timeout_setting: TransferTimeout,
    pub connect_timeout_setting: TransferTimeout,
    pub sockopts: Option<&'a OsString>,
    pub tcp_fastopen: TcpFastOpenMode,
    pub blocking_io: Option<bool>,
}

/// Checks whether the operands request a daemon module listing, and if so, performs it.
///
/// Returns `Some(exit_code)` if a listing was attempted, `None` if transfer should proceed.
pub(super) fn maybe_handle_module_listing<Out, Err>(
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
    inputs: ModuleListingInputs<'_>,
) -> Option<i32>
where
    Out: Write,
    Err: Write,
{
    let ModuleListingInputs {
        file_list_operands,
        remainder,
        daemon_port,
        desired_protocol,
        password_override,
        no_motd,
        address_mode,
        bind_address,
        connect_program,
        remote_shell,
        rsync_path,
        timeout_setting,
        connect_timeout_setting,
        sockopts,
        tcp_fastopen,
        blocking_io,
    } = inputs;

    if !file_list_operands.is_empty() {
        return None;
    }

    let module_list_port = daemon_port.unwrap_or(ModuleListRequest::DEFAULT_PORT);
    let request = match ModuleListRequest::from_operands_with_port(remainder, module_list_port) {
        Ok(Some(request)) => request,
        Ok(None) => return None,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{error}");
            }
            return Some(error.exit_code());
        }
    };

    let request = if let Some(protocol) = desired_protocol {
        request.with_protocol(protocol)
    } else {
        request
    };

    // upstream: main.c - an explicit `-e`/`--rsh` makes `host::` listings reach
    // the daemon over the remote shell (daemon-over-rsh) rather than TCP. Parse
    // the shell spec the same way the transfer path does (config.rs). A
    // malformed spec is ignored, matching the transfer path's lenient handling.
    let remote_shell_args = remote_shell.and_then(|spec| ssh::parse_remote_shell(spec).ok());

    let list_options = ModuleListOptions::default()
        .suppress_motd(no_motd)
        .with_address_mode(address_mode)
        .with_bind_address(bind_address.map(|addr| addr.socket()))
        .with_connect_program(connect_program.cloned())
        .with_remote_shell(remote_shell_args)
        .with_rsync_path(rsync_path.cloned())
        .with_sockopts(sockopts.cloned())
        .with_tcp_fastopen(tcp_fastopen)
        .with_blocking_io(blocking_io);

    match run_module_list_with_password_and_options(
        request,
        list_options,
        password_override,
        timeout_setting,
        connect_timeout_setting,
    ) {
        Ok(list) => {
            if render_module_list(stdout, stderr.writer_mut(), &list, no_motd).is_err() {
                Some(1)
            } else {
                Some(0)
            }
        }
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let code = error.exit_code();
                let _ = writeln!(
                    stderr.writer_mut(),
                    "rsync error: daemon functionality is unavailable in this build (code {code})"
                );
            }
            Some(error.exit_code())
        }
    }
}
