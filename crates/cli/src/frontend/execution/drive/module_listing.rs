use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use rsync_core::client::{
    AddressMode, BindAddress, ModuleListOptions, ModuleListRequest, TransferTimeout,
    run_module_list_with_password_and_options,
};
use rsync_logging::MessageSink;
use rsync_protocol::ProtocolVersion;

use crate::frontend::{
    execution::render_module_list, password::load_optional_password, write_message,
};

pub(super) struct ModuleListingInputs<'a> {
    pub(crate) file_list_operands: &'a [OsString],
    pub(crate) remainder: &'a [OsString],
    pub(crate) daemon_port: Option<u16>,
    pub(crate) desired_protocol: Option<ProtocolVersion>,
    pub(crate) password_file: Option<&'a Path>,
    pub(crate) no_motd: bool,
    pub(crate) address_mode: AddressMode,
    pub(crate) bind_address: Option<&'a BindAddress>,
    pub(crate) connect_program: Option<&'a OsString>,
    pub(crate) timeout_setting: TransferTimeout,
    pub(crate) connect_timeout_setting: TransferTimeout,
}

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
        password_file,
        no_motd,
        address_mode,
        bind_address,
        connect_program,
        timeout_setting,
        connect_timeout_setting,
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
                let _ = writeln!(stderr.writer_mut(), "{}", error);
            }
            return Some(error.exit_code());
        }
    };

    let request = if let Some(protocol) = desired_protocol {
        request.with_protocol(protocol)
    } else {
        request
    };

    let password_override = match load_optional_password(password_file) {
        Ok(secret) => secret,
        Err(message) => {
            if write_message(&message, stderr).is_err() {
                let fallback = message.to_string();
                let _ = writeln!(stderr.writer_mut(), "{}", fallback);
            }
            return Some(1);
        }
    };

    let list_options = ModuleListOptions::default()
        .suppress_motd(no_motd)
        .with_address_mode(address_mode)
        .with_bind_address(bind_address.map(|addr| addr.socket()))
        .with_connect_program(connect_program.cloned());

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
                let _ = writeln!(
                    stderr.writer_mut(),
                    "rsync error: daemon functionality is unavailable in this build (code {})",
                    error.exit_code()
                );
            }
            Some(error.exit_code())
        }
    }
}
