//! Server configuration builders for daemon-mode transfers.
//!
//! Constructs `ServerConfig` instances for receiver (pull) and generator (push)
//! roles, mapping `ClientConfig` options to server-side flags and settings.

use std::ffi::OsString;

use protocol::filters::FilterRuleWireFormat;

use crate::client::config::ClientConfig;
use crate::client::error::{ClientError, invalid_argument_error};
use crate::client::remote::flags;

use crate::server::{ServerConfig, ServerRole};

/// Builds server configuration for receiver role (pull transfer).
pub(crate) fn build_server_config_for_receiver(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Receiver, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    apply_common_daemon_config(config, &mut server_config, filter_rules);
    server_config.reference_directories = config.reference_directories().to_vec();
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
    // no destination implies --list-only. On a pull the local client IS the
    // receiver and list_only is a long-form-only flag absent from the compact
    // letter string, so carry it onto the local receiver config here. The
    // receiver then renders the flist without issuing any per-file NDX request.
    server_config.flags.list_only = config.list_only();

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Builds server configuration for generator role (push transfer).
pub(crate) fn build_server_config_for_generator(
    config: &ClientConfig,
    local_paths: &[String],
    filter_rules: Vec<FilterRuleWireFormat>,
) -> Result<ServerConfig, ClientError> {
    let flag_string = flags::build_server_flag_string(config);
    let args: Vec<OsString> = local_paths.iter().map(OsString::from).collect();

    let mut server_config =
        ServerConfig::from_flag_string_and_args(ServerRole::Generator, flag_string, args)
            .map_err(|e| invalid_argument_error(&format!("invalid server config: {e}"), 1))?;

    apply_common_daemon_config(config, &mut server_config, filter_rules);
    server_config.reference_directories = config.reference_directories().to_vec();

    // upstream: options.c:2476-2501 / main.c:1322-1328 - the local sender
    // resolves a single files-from fd. A local file (LocalFile/Stdin, or a
    // localhost:path hostspec opened locally) is read directly; a remote-
    // hosted list is forwarded from the daemon receiver over the wire and
    // read here as `--files-from=-`.
    let plan = config.files_from().resolve_for(true, config.from0());
    if let Some(path) = plan.sender_files_from_path {
        server_config.file_selection.files_from_path = Some(path);
        server_config.file_selection.from0 = plan.sender_from0;
    }

    flags::apply_common_server_flags(config, &mut server_config);
    Ok(server_config)
}

/// Applies daemon-specific configuration common to both receiver and generator roles.
fn apply_common_daemon_config(
    config: &ClientConfig,
    server_config: &mut ServerConfig,
    filter_rules: Vec<FilterRuleWireFormat>,
) {
    server_config.connection.client_mode = true;
    server_config.connection.is_daemon_connection = true;
    server_config.connection.filter_rules = filter_rules;

    server_config.flags.verbose = config.verbosity() > 0;

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
    // Local-only sender optimization; never emitted onto the wire, so it is
    // carried directly onto the in-process generator's ParsedServerFlags.
    server_config.flags.parallel_delta_scan = config.parallel_delta_scan();

    server_config.write.fsync = config.fsync();
    server_config.write.io_uring_policy = config.io_uring_policy();
    server_config.write.io_uring_depth = config.io_uring_depth();
    server_config.write.zero_copy_policy = config.zero_copy_policy();
    // checksum_choice is set once in `apply_common_server_flags` (called above
    // for both receiver and generator), shared with the SSH transfer paths.
    server_config.connection.compression_level = config.compression_level();

    // upstream: options.c:2704,2800-2805 - replicate the compress flag/option
    // split the client would emit so the daemon-push Generator actually
    // engages the codec. `do_compression` in the transfer layer
    // (transfer/src/lib.rs) reads `flags.compress` for the compact `-z` case
    // and `connection.compress_choice` for everything else. Without this the
    // client-push `TransferConfig` carried neither (it only set compress_choice
    // for explicit choices and never set flags.compress), so both `-z` and
    // `-zz` pushes to a daemon were sent uncompressed.
    if config.compress() {
        let algo = config.compression_algorithm();
        let is_default_zlib = !config.explicit_compress_choice()
            && algo == compress::algorithm::CompressionAlgorithm::default_algorithm();
        if is_default_zlib {
            // upstream: options.c:2704 - the compact `-z` flag drives default
            // zlib through vstring negotiation (no explicit compress_choice).
            server_config.flags.compress = true;
        } else if let Ok(proto_algo) = protocol::CompressionAlgorithm::parse(algo.name()) {
            // upstream: compat.c:543,819 / options.c:2800-2805 - explicit or
            // non-default algorithms (e.g. `-zz` -> zlibx) bypass vstring
            // negotiation and travel as a compress_choice.
            server_config.connection.compress_choice = Some(proto_algo);
        }
    }

    // upstream: options.c:2737-2740 - compress_level defaults to 6 when -z is set.
    if server_config.flags.compress && server_config.connection.compression_level.is_none() {
        server_config.connection.compression_level =
            Some(compress::zlib::CompressionLevel::Default);
    }
    server_config.stop_at = config.stop_at();
}
