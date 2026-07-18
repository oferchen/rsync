//! Server configuration construction for SSH pull (receiver) and push
//! (generator) roles.
//!
//! Translates a [`ClientConfig`] into a [`ServerConfig`], propagating
//! long-form-only flags absent from the compact letter string and wiring
//! `--files-from` for the local sender.

use std::ffi::OsString;

use super::super::super::config::ClientConfig;
use super::super::super::error::{ClientError, invalid_argument_error};
use super::super::flags;
use crate::server::{ServerConfig, ServerRole};

/// Builds server configuration for receiver role (pull transfer).
pub(in crate::client::remote) fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Propagate long-form-only flags that aren't part of the compact flag string.
    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = crate::server::NumericIds::from_client(config.numeric_ids());
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();
    // upstream: options.c:2911-2934 - the alt-dest args (--compare-dest,
    // --copy-dest, --link-dest) live inside the `if (am_sender)` server_options
    // block, so on a pull they are never sent over the wire to the remote
    // sender; the local client IS the receiver and applies them itself in
    // try_dests_reg() (generator.c:954). Carry them onto the local receiver
    // config here so the receiver hard-links / copies / skips unchanged files
    // against the reference dirs. Without this the ssh pull transferred every
    // file whole, while the daemon pull (server_config.rs:30) hard-linked.
    server_config.reference_directories = config.reference_directories().to_vec();
    // upstream: build_server_flag_string no longer packs the compact 'P' letter,
    // and 'D' now tracks devices only, so carry keep_partial and specials onto
    // the local half here (mirrors --partial / --specials|--no-specials which the
    // wire generator emits long-form).
    server_config.flags.partial = config.partial();
    server_config.flags.devices = config.preserve_devices();
    server_config.flags.specials = config.preserve_specials();
    // upstream flist.c:flist_sort_and_clean prunes empty dirs on the receiver
    // (prune_empty_dirs && !am_sender); on a pull the local client IS the receiver,
    // and -m is never sent over the wire (options.c gates it on am_sender), so the
    // flag must be carried onto the local receiver config here.
    server_config.flags.prune_empty_dirs = config.prune_empty_dirs();
    // upstream generator.c:1368-1383 never creates a directory absent at the
    // destination under --existing (ignore_non_existing); on a pull the local
    // client IS the receiver and --existing is a long-form-only flag absent from
    // the compact letter string, so carry it onto the local receiver config here.
    server_config.file_selection.existing_only = config.existing_only();
    // upstream: options.c:2194 / generator.c:1249 - a single source operand with
    // no destination implies list-only. On a pull the local client IS the
    // receiver and `list_only` is a long-form-only concern absent from the
    // compact letter string, so carry it onto the local receiver config here.
    // Without this the receiver renders the flist AND writes files (the compact
    // 'n' is no longer packed for list-only after decoupling it from dry_run).
    server_config.flags.list_only = config.list_only();
    // upstream: options.c:777 / receiver.c:656,1029-1050 - --delay-updates is a
    // plain receiver-side option (no am_sender gate) that stages updates into
    // the partial dir and renames them in the phase-2 sweep. options.c:2886-2892
    // forwards --delay-updates to the remote only on a push (partial_dir &&
    // am_sender); on a pull the local client IS the receiver and the flag is
    // never sent over the wire, so carry it onto the local receiver config here.
    // Without this the receiver updates files in place, defeating --delay-updates.
    server_config.write.delay_updates = config.delay_updates();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
///
/// Propagates `--files-from` plumbing for the local sender (generator) so the
/// file list is built from the requested entry list rather than the source
/// directory's full tree walk.
///
/// # Upstream Reference
///
/// - `options.c:2465-2510` - the sender opens a local files-from file (or
///   sets up filesfrom_fd for remote/stdin sources).
/// - `flist.c:2275-2298` - `send_file_list()` chdirs to `argv[0]` then reads
///   filenames from `filesfrom_fd` to emit the file list.
/// - `main.c:1322-1328` - when `filesfrom_host` is non-NULL, the sender
///   wires `filesfrom_fd = f_in` so the remote forwards bytes via the wire.
pub(in crate::client::remote) fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    // Propagate long-form-only flags that aren't part of the compact flag string.
    // upstream: numeric_ids and delete are --numeric-ids / --delete-* long-form args only.
    server_config.flags.numeric_ids = crate::server::NumericIds::from_client(config.numeric_ids());
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();
    // upstream: build_server_flag_string no longer packs the compact 'P' letter,
    // and 'D' now tracks devices only, so carry keep_partial and specials onto
    // the local half here (mirrors --partial / --specials|--no-specials which the
    // wire generator emits long-form).
    server_config.flags.partial = config.partial();
    server_config.flags.devices = config.preserve_devices();
    server_config.flags.specials = config.preserve_specials();

    apply_files_from_for_sender(config, &mut server_config);

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Wires `--files-from` into a sender (`Generator`) server configuration.
///
/// The local sender resolves entries relative to `argv[0]` (the first transfer
/// operand) and emits a file list constrained to those entries instead of
/// walking the entire source tree. Without this wiring the generator would
/// recurse the absolute source directory and (under `--relative`, implied by
/// `--files-from`) mirror its absolute path on the destination - the exact
/// failure mode that surfaces in the upstream `files-from.test` SSH-push
/// invocation.
///
/// # Upstream Reference
///
/// - `options.c:2473` - `filesfrom_fd = 0` for `--files-from=-` (stdin).
/// - `options.c:2501` - `filesfrom_fd = open(files_from, O_RDONLY|O_BINARY)`
///   for local files.
/// - `main.c:1322-1328` - remote files-from wires `filesfrom_fd = f_in` after
///   `setup_protocol()`; the remote receiver forwards the list bytes over the
///   wire via `start_filesfrom_forwarding`.
fn apply_files_from_for_sender(config: &ClientConfig, server_config: &mut ServerConfig) {
    // upstream: options.c:2476-2501 / main.c:1322-1328 - the local sender
    // resolves a single files-from fd. A localhost:path hostspec is opened
    // locally here (never staged + wire-forwarded), matching a plain local
    // file; a remote-hosted list is read from the wire as `--files-from=-`.
    let plan = config.files_from().resolve_for(true, config.from0());
    if let Some(path) = plan.sender_files_from_path {
        server_config.file_selection.files_from_path = Some(path);
        server_config.file_selection.from0 = plan.sender_from0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::config::ReferenceDirectoryKind;

    /// On an ssh pull the local client IS the receiver, and the alt-dest args
    /// (--compare-dest / --copy-dest / --link-dest) are never sent over the wire
    /// to the remote sender (upstream options.c:2911-2934 gates them on
    /// am_sender). The receiver applies them itself in try_dests_reg()
    /// (generator.c:954), so the ssh receiver config must carry them locally -
    /// exactly as the daemon receiver builder does. Regression guard for the ssh
    /// pull that hard-linked nothing because reference_directories was empty
    /// while local and daemon pulls hard-linked correctly.
    #[test]
    fn receiver_config_propagates_reference_directories() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.reference_directories.len(), 2);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            server_config.reference_directories[0]
                .path()
                .to_str()
                .unwrap(),
            "/tmp/compare"
        );
        assert_eq!(
            server_config.reference_directories[1].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            server_config.reference_directories[1]
                .path()
                .to_str()
                .unwrap(),
            "/prev"
        );
    }

    /// Without any alt-dest option the receiver config carries no reference
    /// directories, so the hard-link/copy/skip path stays disabled and every
    /// file transfers as before.
    #[test]
    fn receiver_config_without_alt_dest_has_no_reference_directories() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();
        assert!(server_config.reference_directories.is_empty());
    }
}
