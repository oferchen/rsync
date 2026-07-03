//! SSH transfer orchestration.
//!
//! This module coordinates SSH-based remote transfers by spawning SSH connections,
//! negotiating the rsync protocol, and executing transfers using the server
//! infrastructure. It mirrors the flow in upstream `main.c:do_cmd()` where the
//! client forks the remote shell, sets up pipes, and dispatches to the sender
//! or receiver role.
//!
//! # Architecture
//!
//! Transfers use the `SshConnection::split` method to obtain separate read/write
//! halves, which are then passed to the server infrastructure for protocol handling.
//!
//! # Submodules
//!
//! - `drive` - Public entry point and push/pull/proxy transfer orchestration
//! - `parse` - Remote operand parsing and invocation argument construction
//! - `connection` - SSH connection spawn and secluded-args plumbing
//! - `server_config` - Receiver/generator server configuration construction
//! - `exit_status` - Server-stats-to-summary and child exit-status mapping
//! - `progress` - Progress observer adaptation
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH fork/exec and pipe setup
//! - `main.c:client_run()` - Role dispatch after SSH connection
//! - `options.c:server_options()` - Remote `--server` argument construction

mod connection;
mod drive;
mod exit_status;
mod parse;
mod progress;
mod server_config;

pub use drive::run_ssh_transfer;
pub(super) use exit_status::{format_stderr_context, map_child_exit_status};

#[cfg(any(feature = "async-ssh", feature = "embedded-ssh"))]
pub(super) use exit_status::convert_server_stats_to_summary;
#[cfg(feature = "async-ssh")]
pub(super) use parse::{parse_remote_operands, parse_single_remote};
#[cfg(any(test, feature = "async-ssh"))]
pub(super) use server_config::{
    build_server_config_for_generator, build_server_config_for_receiver,
};

#[cfg(test)]
mod tests {
    use super::*;
    use connection::{should_warn_double_compression, warn_double_compression_once};

    use super::super::super::config::ClientConfig;
    use crate::server::ServerRole;

    #[test]
    fn builds_receiver_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_receiver(&config, &["dest/".to_owned()]);
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Receiver);
        assert_eq!(server_config.args.len(), 1);
        assert_eq!(server_config.args[0], "dest/");
    }

    #[test]
    fn receiver_server_config_propagates_prune_empty_dirs() {
        // upstream prunes empty dirs on the receiver (flist.c: prune_empty_dirs &&
        // !am_sender); on a pull the local client IS the receiver and -m is never
        // sent over the wire, so the flag must be carried onto the receiver config
        // here or prune_empty_dirs_pass never runs. Regression guard for the
        // remote-shell pull prune gap.
        let config = ClientConfig::builder()
            .recursive(true)
            .times(true)
            .prune_empty_dirs(true)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest/".to_owned()]).unwrap();
        assert!(server_config.flags.prune_empty_dirs);
    }

    #[test]
    fn receiver_server_config_prune_empty_dirs_default_false() {
        let config = ClientConfig::builder().recursive(true).times(true).build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest/".to_owned()]).unwrap();
        assert!(!server_config.flags.prune_empty_dirs);
    }

    #[test]
    fn builds_generator_server_config() {
        let config = ClientConfig::builder().recursive(true).times(true).build();

        let result = build_server_config_for_generator(
            &config,
            &["file1.txt".to_owned(), "file2.txt".to_owned()],
        );
        assert!(result.is_ok());

        let server_config = result.unwrap();
        assert_eq!(server_config.role, ServerRole::Generator);
        assert_eq!(server_config.args.len(), 2);
        assert_eq!(server_config.args[0], "file1.txt");
        assert_eq!(server_config.args[1], "file2.txt");
    }

    /// UTS files-from SSH push regression: the local sender (Generator) must
    /// learn the local `--files-from` path so its generator reads entry names
    /// from the requested list instead of recursing the source operand and
    /// (under implied `--relative`) mirroring its absolute path on the
    /// destination.
    ///
    /// upstream: `options.c:2501 filesfrom_fd = open(files_from, ...)`,
    /// `flist.c:2275-2298` send_file_list() walking the open fd.
    #[test]
    fn generator_config_sets_files_from_path_for_local_file_push() {
        use super::super::super::config::FilesFromSource;
        use std::path::PathBuf;

        let list_path = PathBuf::from("/tmp/filelist.txt");
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(list_path.clone()))
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some(list_path.to_string_lossy().as_ref()),
            "SSH push must point the local generator at the local --files-from \
             file so entries are emitted with relative wire-side names"
        );
    }

    /// SSH push with stdin-sourced `--files-from`: the local sender reads
    /// filenames from its standard input. The transfer crate signals this with
    /// the sentinel path "-" mirroring upstream's `options.c:2473
    /// filesfrom_fd = 0` assignment.
    #[test]
    fn generator_config_sets_files_from_path_for_stdin_push() {
        use super::super::super::config::FilesFromSource;

        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .from0(true)
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-")
        );
        assert!(server_config.file_selection.from0);
    }

    /// SSH push with remote-sourced `--files-from`: the local sender consumes
    /// the list bytes forwarded by the remote receiver over the wire. The
    /// transfer crate's protocol stream is the "-" sentinel here too;
    /// upstream wires this via `main.c:1322-1328 filesfrom_fd = f_in`.
    #[test]
    fn generator_config_sets_files_from_stdin_for_remote_push() {
        use super::super::super::config::FilesFromSource;

        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-"),
            "remote --files-from is read from the wire on the local sender"
        );
        assert!(server_config.file_selection.from0);
    }

    /// SSH push baseline: when `--files-from` is not configured the generator
    /// performs its usual recursive walk. The `files_from_path` field stays
    /// empty so the engine falls back to `build_file_list(paths)`.
    #[test]
    fn generator_config_leaves_files_from_path_unset_when_disabled() {
        let config = ClientConfig::builder().recursive(true).build();

        let server_config = build_server_config_for_generator(&config, &["/tmp/source".to_owned()])
            .expect("generator config builds");

        assert!(
            server_config.file_selection.files_from_path.is_none(),
            "no --files-from must leave files_from_path unset"
        );
    }

    #[test]
    fn warns_on_double_compression() {
        // Both rsync --compress and SSH -C engaged: the predicate fires.
        assert!(should_warn_double_compression(true, true));
        // The one-shot emitter is safe to call; the first eligible call wins
        // process-wide and subsequent calls become no-ops. We only assert that
        // it does not panic or hang.
        warn_double_compression_once(true, true);
        warn_double_compression_once(true, true);
    }

    #[test]
    fn no_warning_when_only_rsync_compress() {
        assert!(!should_warn_double_compression(true, false));
        // Calling the emitter must be a no-op (no panic, no state change).
        warn_double_compression_once(true, false);
    }

    #[test]
    fn no_warning_when_only_ssh_compress() {
        assert!(!should_warn_double_compression(false, true));
        warn_double_compression_once(false, true);
    }

    #[test]
    fn no_warning_when_neither_compresses() {
        assert!(!should_warn_double_compression(false, false));
        warn_double_compression_once(false, false);
    }

    #[test]
    fn format_stderr_context_empty_input() {
        assert_eq!(format_stderr_context(&[]), "");
    }

    #[test]
    fn format_stderr_context_whitespace_only() {
        assert_eq!(format_stderr_context(b"  \n\n  "), "");
    }

    #[test]
    fn format_stderr_context_single_line() {
        let output = format_stderr_context(b"Permission denied (publickey).\n");
        assert_eq!(output, "\nSSH stderr:\nPermission denied (publickey).");
    }

    #[test]
    fn format_stderr_context_multi_line() {
        let input = b"Warning: Permanently added 'host' to known hosts.\nrsync error: some error\n";
        let output = format_stderr_context(input);
        assert!(output.starts_with("\nSSH stderr:\n"));
        assert!(output.contains("Warning: Permanently added"));
        assert!(output.contains("rsync error: some error"));
    }

    #[test]
    fn format_stderr_context_invalid_utf8() {
        let input = b"error: \xff\xfe bad bytes\n";
        let output = format_stderr_context(input);
        assert!(output.starts_with("\nSSH stderr:\n"));
        assert!(output.contains("error:"));
    }

    #[cfg(unix)]
    mod child_exit_status_tests {
        use super::*;
        use crate::exit_code::ExitCode;

        #[cfg(unix)]
        fn exit_status_for_code(code: i32) -> std::process::ExitStatus {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("exit {code}"))
                .status()
                .expect("failed to run sh")
        }

        #[cfg(unix)]
        #[test]
        fn maps_success_to_ok() {
            let status = exit_status_for_code(0);
            assert_eq!(map_child_exit_status(status), ExitCode::Ok);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_127_to_command_not_found() {
            let status = exit_status_for_code(127);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandNotFound);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_126_to_command_run() {
            let status = exit_status_for_code(126);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandRun);
        }

        #[cfg(unix)]
        #[test]
        fn maps_exit_255_to_command_failed() {
            let status = exit_status_for_code(255);
            assert_eq!(map_child_exit_status(status), ExitCode::CommandFailed);
        }

        #[cfg(unix)]
        #[test]
        fn maps_rsync_exit_code_23_to_partial_transfer() {
            let status = exit_status_for_code(23);
            assert_eq!(map_child_exit_status(status), ExitCode::PartialTransfer);
        }

        #[cfg(unix)]
        #[test]
        fn maps_rsync_exit_code_24_to_vanished() {
            let status = exit_status_for_code(24);
            assert_eq!(map_child_exit_status(status), ExitCode::Vanished);
        }

        #[cfg(unix)]
        #[test]
        fn maps_unknown_exit_code_to_partial_transfer() {
            let status = exit_status_for_code(42);
            assert_eq!(map_child_exit_status(status), ExitCode::PartialTransfer);
        }

        #[cfg(unix)]
        #[test]
        fn maps_signal_killed_to_command_killed() {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg("kill -9 $$")
                .spawn()
                .expect("spawn");
            let status = child.wait().expect("wait");
            assert_eq!(map_child_exit_status(status), ExitCode::CommandKilled);
        }
    }
}
