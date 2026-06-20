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
    server_config.flags.numeric_ids = config.numeric_ids();
    server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();
    server_config.file_selection.size_only = config.size_only();

    server_config.write.fsync = config.fsync();
    server_config.write.adaptive_concurrency = config.adaptive_concurrency();
    server_config.write.io_uring_policy = config.io_uring_policy();
    server_config.write.io_uring_depth = config.io_uring_depth();
    server_config.write.zero_copy_policy = config.zero_copy_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
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
