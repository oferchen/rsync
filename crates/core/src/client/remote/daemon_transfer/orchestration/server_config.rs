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

    // upstream: clientserver.c - when --files-from references a remote file
    // (colon prefix), the daemon receiver opens the file locally and forwards
    // its content to the client sender via start_filesfrom_forwarding().
    if config.files_from().is_remote() {
        server_config.file_selection.files_from_path = Some("-".to_owned());
        server_config.file_selection.from0 = true;
    }

    // upstream: options.c:2944 - when the client is the sender and --files-from
    // points to a local file, the sender reads the list directly.
    use crate::client::config::FilesFromSource;
    match config.files_from() {
        FilesFromSource::LocalFile(path) => {
            server_config.file_selection.files_from_path = Some(path.to_string_lossy().to_string());
            server_config.file_selection.from0 = config.from0();
        }
        FilesFromSource::Stdin => {
            server_config.file_selection.files_from_path = Some("-".to_owned());
            server_config.file_selection.from0 = config.from0();
        }
        _ => {}
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
    server_config.write.io_uring_policy = config.io_uring_policy();
    server_config.checksum_choice = config.checksum_protocol_override();
    server_config.connection.compression_level = config.compression_level();

    // upstream: compat.c:543,819 - when --compress-choice is explicitly set,
    // bypass vstring negotiation and use the specified algorithm. Convert from
    // compress crate enum to protocol crate enum via the canonical wire name.
    if config.explicit_compress_choice() {
        let algo = config.compression_algorithm();
        if let Ok(proto_algo) = protocol::CompressionAlgorithm::parse(algo.name()) {
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
