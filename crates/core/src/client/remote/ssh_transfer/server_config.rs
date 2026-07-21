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
    // upstream: backup.c:make_backup() runs on the receiver, invoked from
    // generator.c/receiver.c. `make_backups` rides in the compact flag string as
    // 'b' (options.c:2630-2631), so flags.backup is already set here; but
    // --backup-dir / --suffix are long-form values finalized in the local popt
    // parse (options.c:2285-2298) and never delivered onto the receiver config.
    // On a pull the local client IS the receiver, so carry backup_dir/backup_suffix
    // here - otherwise effective_backup_suffix() falls back to "~" and the backup
    // lands beside the file instead of in --backup-dir (local/daemon pulls kept
    // them and behaved correctly).
    server_config.backup_dir = config.backup_directory().map(|p| p.display().to_string());
    server_config.backup_suffix = config
        .backup_suffix()
        .map(|s| s.to_string_lossy().into_owned());
    // upstream: --chmod is parsed into `chmod_modes` (options.c:1762) and is
    // never placed in server_options, so it is never forwarded to the remote
    // sender. On a pull the local client IS the receiver and applies the
    // modifiers itself as it reads each incoming flist entry (flist.c:905-906
    // recv_file_entry() -> tweak_mode()). Carry them onto the local receiver
    // config here; without this the ssh pull left every regular file at its
    // source mode while local copies applied --chmod correctly.
    server_config.chmod = config.chmod().cloned();
    // upstream: options.c:2996-2997 - `--mkpath` is forwarded to the remote only
    // inside the `if (am_sender)` server_options block, so on a pull it never
    // rides the wire; the local client IS the receiver and creates the dest-arg
    // path chain itself in get_local_name() (main.c:736 make_path under mkpath).
    // Carry it onto the local receiver config here. Without this the ssh pull to
    // a missing deep destination failed with "failed to create destination root
    // ... No such file or directory" while local copies honored --mkpath.
    server_config.flags.mkpath = config.mkpath();
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
    // upstream generator.c:1395 skips any file already present at the destination
    // under --ignore-existing (`if (ignore_existing > 0 && statret == 0)` early
    // goto cleanup). options.c:2911-2919 forwards --ignore-existing to the remote
    // only inside the `if (am_sender)` server_options block, so on a pull it is
    // never sent over the wire; the local client IS the receiver and applies it
    // itself. Carry it onto the local receiver config here, mirroring
    // existing_only above. Without this the ssh pull re-transferred and
    // overwrote existing destination files instead of skipping them.
    server_config.file_selection.ignore_existing = config.ignore_existing();
    // upstream: options.c:2907-2909 forwards --temp-dir to the remote only inside
    // the `if (am_sender)` server_options block, so on a pull it is never sent
    // over the wire; the local client IS the receiver and stages the temp file
    // itself (receiver.c:766 open_tmpfile() honours tmpdir). Carry it onto the
    // local receiver config here - without this the ssh pull staged the temp file
    // in the destination directory, ignoring --temp-dir (local copies honoured it).
    server_config.temp_dir = config.temp_directory().map(std::path::Path::to_path_buf);
    // upstream rsync.c:583 adds ATTRS_SKIP_MTIME for `omit_dir_times && S_ISDIR`,
    // and generator.c:2271 gates need_retouch_dir_times on !omit_dir_times.
    // options.c:2646-2647 packs the compact 'O' into server_options only when
    // am_sender, so on a pull -O never rides the wire; the local client IS the
    // receiver and must apply it itself. Carry it onto the local receiver config
    // here - without this the ssh pull set directory mtimes from the source while
    // local copies left them at the current time.
    server_config.flags.omit_dir_times = config.omit_dir_times();
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
    // upstream: --chmod is parsed into `chmod_modes` (options.c:1762) and is
    // never placed in server_options, so it is never forwarded to the remote
    // receiver. On a push the local client IS the sender and applies the
    // modifiers itself as it builds each outgoing flist entry (flist.c:1580-1581
    // send_file_name() -> tweak_mode()). Carry them onto the local generator
    // config here; without this the ssh push left every file at its source mode
    // while local copies and pulls applied --chmod correctly.
    server_config.chmod = config.chmod().cloned();

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

    /// On an ssh pull the local client IS the receiver and runs
    /// backup.c:make_backup() itself. `make_backups` rides in the compact 'b'
    /// letter, but --backup-dir / --suffix are long-form values finalized in the
    /// local popt parse (upstream options.c:2285-2298) and must be carried onto
    /// the receiver config. Regression guard for the ssh pull that wrote a "~"
    /// backup beside the file because backup_dir/backup_suffix were empty while
    /// local and daemon pulls placed the backup in --backup-dir.
    #[test]
    fn receiver_config_propagates_backup_dir_and_suffix() {
        let config = ClientConfig::builder()
            .backup(true)
            .backup_directory(Some("/bak"))
            .backup_suffix(Some(".old"))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.backup);
        assert_eq!(server_config.backup_dir.as_deref(), Some("/bak"));
        assert_eq!(server_config.backup_suffix.as_deref(), Some(".old"));
    }

    /// Without --backup the receiver config carries no backup directory or
    /// suffix, so the backup path stays disabled.
    #[test]
    fn receiver_config_without_backup_has_no_backup_dir() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.flags.backup);
        assert!(server_config.backup_dir.is_none());
        assert!(server_config.backup_suffix.is_none());
    }

    /// On an ssh pull the local client IS the receiver and applies
    /// --ignore-existing itself (upstream generator.c:1395 skips existing dest
    /// files). options.c:2911-2919 forwards the flag to the remote only when
    /// am_sender, so on a pull it never rides the wire and must be carried onto
    /// the receiver config. Regression guard for the ssh pull that overwrote an
    /// existing destination file instead of skipping it.
    #[test]
    fn receiver_config_propagates_ignore_existing() {
        let config = ClientConfig::builder().ignore_existing(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.file_selection.ignore_existing);
    }

    /// Without --ignore-existing the receiver config leaves the flag clear, so
    /// normal transfers still update existing destination files.
    #[test]
    fn receiver_config_without_ignore_existing_stays_clear() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.file_selection.ignore_existing);
    }

    /// On an ssh pull the local client IS the receiver and applies `--chmod`
    /// itself. `--chmod` is never forwarded to the remote sender (upstream
    /// options.c:1762 parses it into `chmod_modes`, absent from server_options),
    /// so the receiver applies it as it reads each flist entry
    /// (flist.c:905-906). The receiver config must carry the parsed modifiers.
    /// Regression guard for the ssh pull that left files at their source mode
    /// while local copies applied `--chmod`.
    #[test]
    fn receiver_config_propagates_chmod() {
        let modifiers = ::metadata::ChmodModifiers::parse("D2755,F640").expect("parse chmod spec");
        let config = ClientConfig::builder()
            .chmod(Some(modifiers.clone()))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(server_config.chmod.as_ref(), Some(&modifiers));
    }

    /// Without `--chmod` the receiver config carries no chmod modifiers, so the
    /// destination mode is preserved exactly as sent.
    #[test]
    fn receiver_config_without_chmod_has_none() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.chmod.is_none());
    }

    /// On an ssh pull the local client IS the receiver and stages the temp file
    /// itself (upstream receiver.c:766 open_tmpfile() honours tmpdir).
    /// options.c:2907-2909 forwards --temp-dir to the remote only when am_sender,
    /// so on a pull it never rides the wire and must be carried onto the receiver
    /// config. Regression guard for the ssh pull that staged temps in the
    /// destination directory instead of --temp-dir.
    #[test]
    fn receiver_config_propagates_temp_dir() {
        let config = ClientConfig::builder()
            .temp_directory(Some("/var/tmp/rsync"))
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert_eq!(
            server_config.temp_dir.as_deref(),
            Some(std::path::Path::new("/var/tmp/rsync"))
        );
    }

    /// Without --temp-dir the receiver config leaves temp_dir unset, so temps
    /// stage alongside the destination exactly as before.
    #[test]
    fn receiver_config_without_temp_dir_stays_none() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.temp_dir.is_none());
    }

    /// On an ssh pull the local client IS the receiver and applies
    /// --omit-dir-times itself (upstream rsync.c:583 skips a directory's mtime,
    /// generator.c:2271 gates the retouch pass). options.c:2646-2647 packs the
    /// compact 'O' only when am_sender, so on a pull it never rides the wire and
    /// must be carried onto the receiver config. Regression guard for the ssh
    /// pull that set directory mtimes from the source.
    #[test]
    fn receiver_config_propagates_omit_dir_times() {
        let config = ClientConfig::builder().omit_dir_times(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.omit_dir_times);
    }

    /// Without --omit-dir-times the receiver config leaves the flag clear, so
    /// directory mtimes are preserved as before.
    #[test]
    fn receiver_config_without_omit_dir_times_stays_clear() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.flags.omit_dir_times);
    }

    /// On an ssh pull the local client IS the receiver and creates the dest-arg
    /// path chain itself. `--mkpath` is forwarded to the remote only when
    /// am_sender (upstream options.c:2996-2997), so on a pull it never rides the
    /// wire and must be carried onto the receiver config. Regression guard for
    /// the ssh pull that failed with "failed to create destination root ... No
    /// such file or directory" against a missing deep destination while local
    /// copies honored --mkpath.
    #[test]
    fn receiver_config_propagates_mkpath() {
        let config = ClientConfig::builder().mkpath(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(server_config.flags.mkpath);
    }

    /// Without `--mkpath` the receiver config leaves the flag clear, so a missing
    /// destination parent stays a fatal error, matching upstream main.c:787.
    #[test]
    fn receiver_config_without_mkpath_stays_clear() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()]).unwrap();

        assert!(!server_config.flags.mkpath);
    }

    /// On an ssh push the local client IS the sender and applies `--chmod`
    /// itself as it builds each outgoing flist entry (upstream flist.c:1580-1581
    /// send_file_name() -> tweak_mode()). `--chmod` is never forwarded to the
    /// remote receiver, so the generator config must carry the parsed modifiers.
    /// Regression guard for the ssh push that left files at their source mode
    /// while local copies and pulls applied `--chmod`.
    #[test]
    fn generator_config_propagates_chmod() {
        let modifiers = ::metadata::ChmodModifiers::parse("D2755,F640").expect("parse chmod spec");
        let config = ClientConfig::builder()
            .chmod(Some(modifiers.clone()))
            .build();
        let server_config =
            build_server_config_for_generator(&config, &["/tmp/source".to_owned()]).unwrap();

        assert_eq!(server_config.chmod.as_ref(), Some(&modifiers));
    }

    /// Without `--chmod` the generator config carries no chmod modifiers, so the
    /// source mode travels unchanged.
    #[test]
    fn generator_config_without_chmod_has_none() {
        let config = ClientConfig::builder().build();
        let server_config =
            build_server_config_for_generator(&config, &["/tmp/source".to_owned()]).unwrap();

        assert!(server_config.chmod.is_none());
    }
}
