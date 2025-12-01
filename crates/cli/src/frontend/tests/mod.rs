#![allow(unused_imports)]

use super::server;
use super::*;
use crate::password::set_password_stdin_input;
use checksums::strong::Md5;
use core::fallback::CLIENT_FALLBACK_ENV;
use core::{
    branding::manifest,
    client::{ClientEventKind, FilterRuleKind},
};
use daemon as daemon_cli;
use filters::{FilterRule as EngineFilterRule, FilterSet};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

#[path = "acls.rs"]
mod acls_tests;
#[path = "apply.rs"]
mod apply_tests;
#[path = "archive.rs"]
mod archive_tests;
#[path = "backup.rs"]
mod backup_tests;
#[path = "bind.rs"]
mod bind_tests;
#[path = "bwlimit.rs"]
mod bwlimit_tests;
#[path = "checksum.rs"]
mod checksum_tests;
#[path = "chown.rs"]
mod chown_tests;
#[path = "clap.rs"]
mod clap_tests;
#[path = "collect.rs"]
mod collect_tests;
#[path = "combined.rs"]
mod combined_tests;
mod common;
#[path = "compression.rs"]
mod compression_tests;
#[path = "connect.rs"]
mod connect_tests;
#[path = "daemon.rs"]
mod daemon_tests;
#[path = "delete.rs"]
mod delete_tests;
#[path = "dry_run.rs"]
mod dry_run_tests;
#[path = "files_from.rs"]
mod files_from_tests;
#[path = "format.rs"]
mod format_tests;
#[path = "h.rs"]
mod h_tests;
#[path = "help.rs"]
mod help_tests;
#[path = "iconv.rs"]
mod iconv_tests;
#[path = "info_debug.rs"]
mod info_debug_tests;
#[path = "invalid.rs"]
mod invalid_tests;
#[path = "list_only.rs"]
mod list_only_tests;
#[path = "load.rs"]
mod load_tests;
#[path = "log_file.rs"]
mod log_file_tests;
#[path = "long.rs"]
mod long_tests;
#[path = "merge.rs"]
mod merge_tests;
#[path = "module.rs"]
mod module_tests;
#[path = "non.rs"]
mod non_tests;
#[path = "operands.rs"]
mod operands_tests;
#[path = "out/mod.rs"]
mod out_tests;
#[path = "outbuf.rs"]
mod outbuf_tests;
#[path = "parse_args_allows.rs"]
mod parse_args_allows_tests;
#[path = "parse_args_captures.rs"]
mod parse_args_captures_tests;
#[path = "parse_args_collects.rs"]
mod parse_args_collects_tests;
#[path = "parse_args_compress.rs"]
mod parse_args_compress_tests;
#[path = "parse_args_no.rs"]
mod parse_args_no_tests;
#[path = "parse_args_reads.rs"]
mod parse_args_reads_tests;
#[path = "parse_args_recognises_append.rs"]
mod parse_args_recognises_append_tests;
#[path = "parse_args_recognises_archive.rs"]
mod parse_args_recognises_archive_tests;
#[path = "parse_args_recognises_batch.rs"]
mod parse_args_recognises_batch_tests;
#[path = "parse_args_recognises_block_size.rs"]
mod parse_args_recognises_block_size_tests;
#[path = "parse_args_recognises_blocking.rs"]
mod parse_args_recognises_blocking_tests;
#[path = "parse_args_recognises_checksum.rs"]
mod parse_args_recognises_checksum_tests;
#[path = "parse_args_recognises_chown.rs"]
mod parse_args_recognises_chown_tests;
#[path = "parse_args_recognises_compress.rs"]
mod parse_args_recognises_compress_tests;
#[path = "parse_args_recognises_copy.rs"]
mod parse_args_recognises_copy_tests;
#[path = "parse_args_recognises_cvs.rs"]
mod parse_args_recognises_cvs_tests;
#[path = "parse_args_recognises_delay.rs"]
mod parse_args_recognises_delay_tests;
#[path = "parse_args_recognises_devices.rs"]
mod parse_args_recognises_devices_tests;
#[path = "parse_args_recognises_executability.rs"]
mod parse_args_recognises_executability_tests;
#[path = "parse_args_recognises_existing.rs"]
mod parse_args_recognises_existing_tests;
#[path = "parse_args_recognises_force.rs"]
mod parse_args_recognises_force_tests;
#[path = "parse_args_recognises_fsync.rs"]
mod parse_args_recognises_fsync_tests;
#[path = "parse_args_recognises_fuzzy.rs"]
mod parse_args_recognises_fuzzy_tests;
#[path = "parse_args_recognises_group.rs"]
mod parse_args_recognises_group_tests;
#[path = "parse_args_recognises_hard.rs"]
mod parse_args_recognises_hard_tests;
#[path = "parse_args_recognises_human.rs"]
mod parse_args_recognises_human_tests;
#[path = "parse_args_recognises_iconv.rs"]
mod parse_args_recognises_iconv_tests;
#[path = "parse_args_recognises_ignore_times.rs"]
mod parse_args_recognises_ignore_times_tests;
#[path = "parse_args_recognises_implied.rs"]
mod parse_args_recognises_implied_tests;
#[path = "parse_args_recognises_inplace.rs"]
mod parse_args_recognises_inplace_tests;
#[path = "parse_args_recognises_itemize.rs"]
mod parse_args_recognises_itemize_tests;
#[path = "parse_args_recognises_keep.rs"]
mod parse_args_recognises_keep_tests;
#[path = "parse_args_recognises_list.rs"]
mod parse_args_recognises_list_tests;
#[path = "parse_args_recognises_log.rs"]
mod parse_args_recognises_log_tests;
#[path = "parse_args_recognises_mkpath.rs"]
mod parse_args_recognises_mkpath_tests;
#[path = "parse_args_recognises_modify.rs"]
mod parse_args_recognises_modify_tests;
#[path = "parse_args_recognises_msgs2stderr.rs"]
mod parse_args_recognises_msgs2stderr_tests;
#[path = "parse_args_recognises_no.rs"]
mod parse_args_recognises_no_tests;
#[path = "parse_args_recognises_numeric.rs"]
mod parse_args_recognises_numeric_tests;
#[path = "parse_args_recognises_one.rs"]
mod parse_args_recognises_one_tests;
#[path = "parse_args_recognises_outbuf.rs"]
mod parse_args_recognises_outbuf_tests;
#[path = "parse_args_recognises_owner.rs"]
mod parse_args_recognises_owner_tests;
#[path = "parse_args_recognises_partial.rs"]
mod parse_args_recognises_partial_tests;
#[path = "parse_args_recognises_password.rs"]
mod parse_args_recognises_password_tests;
#[path = "parse_args_recognises_perms.rs"]
mod parse_args_recognises_perms_tests;
#[path = "parse_args_recognises_port.rs"]
mod parse_args_recognises_port_tests;
#[path = "parse_args_recognises_preallocate.rs"]
mod parse_args_recognises_preallocate_tests;
#[path = "parse_args_recognises_prune.rs"]
mod parse_args_recognises_prune_tests;
#[path = "parse_args_recognises_recursive.rs"]
mod parse_args_recognises_recursive_tests;
#[path = "parse_args_recognises_relative.rs"]
mod parse_args_recognises_relative_tests;
#[path = "parse_args_recognises_remove.rs"]
mod parse_args_recognises_remove_tests;
#[path = "parse_args_recognises_safe.rs"]
mod parse_args_recognises_safe_tests;
#[path = "parse_args_recognises_sockopts.rs"]
mod parse_args_recognises_sockopts_tests;
#[path = "parse_args_recognises_sparse.rs"]
mod parse_args_recognises_sparse_tests;
#[path = "parse_args_recognises_specials.rs"]
mod parse_args_recognises_specials_tests;
#[path = "parse_args_recognises_stats.rs"]
mod parse_args_recognises_stats_tests;
#[path = "parse_args_recognises_stop.rs"]
mod parse_args_recognises_stop_tests;
#[path = "parse_args_recognises_super.rs"]
mod parse_args_recognises_super_tests;
#[path = "parse_args_recognises_temp.rs"]
mod parse_args_recognises_temp_tests;
#[path = "parse_args_recognises_update.rs"]
mod parse_args_recognises_update_tests;
#[path = "parse_args_recognises_whole.rs"]
mod parse_args_recognises_whole_tests;
#[path = "parse_args_records.rs"]
mod parse_args_records_tests;
#[path = "parse_args_rejects.rs"]
mod parse_args_rejects_tests;
#[path = "parse_args_resets.rs"]
mod parse_args_resets_tests;
#[path = "parse_args_respects.rs"]
mod parse_args_respects_tests;
#[path = "parse_args_sets.rs"]
mod parse_args_sets_tests;
#[path = "parse_checksum.rs"]
mod parse_checksum_tests;
#[path = "parse_compress.rs"]
mod parse_compress_tests;
#[path = "parse_filter.rs"]
mod parse_filter_tests;
#[path = "parse_max.rs"]
mod parse_max_tests;
#[path = "parse_modify.rs"]
mod parse_modify_tests;
#[path = "parse_size.rs"]
mod parse_size_tests;
#[path = "password.rs"]
mod password_tests;
#[path = "pow.rs"]
mod pow_tests;
#[path = "process.rs"]
mod process_tests;
#[path = "progress_render.rs"]
mod progress_render_tests;
#[path = "progress.rs"]
mod progress_tests;
#[path = "protocol.rs"]
mod protocol_tests;
#[path = "remote.rs"]
mod remote_tests;
#[path = "rsync.rs"]
mod rsync_tests;
#[path = "run.rs"]
mod run_tests;
// Disabled temporarily due to missing server module exports:
// #[path = "server.rs"]
// mod server_tests;
#[path = "short.rs"]
mod short_tests;
#[path = "size.rs"]
mod size_tests;
#[path = "stats.rs"]
mod stats_tests;
#[path = "stop.rs"]
mod stop_tests;
#[path = "timeout.rs"]
mod timeout_tests;
#[path = "transfer_request_copies.rs"]
mod transfer_request_copies_tests;
#[path = "transfer_request_reports.rs"]
mod transfer_request_reports_tests;
#[path = "transfer_request_with_archive.rs"]
mod transfer_request_with_archive_tests;
#[path = "transfer_request_with_bwlimit.rs"]
mod transfer_request_with_bwlimit_tests;
#[path = "transfer_request_with_cvs.rs"]
mod transfer_request_with_cvs_tests;
#[path = "transfer_request_with_delete.rs"]
mod transfer_request_with_delete_tests;
#[path = "transfer_request_with_exclude.rs"]
mod transfer_request_with_exclude_tests;
#[path = "transfer_request_with_executability.rs"]
mod transfer_request_with_executability_tests;
#[path = "transfer_request_with_files_from.rs"]
mod transfer_request_with_files_from_tests;
#[path = "transfer_request_with_files.rs"]
mod transfer_request_with_files_tests;
#[path = "transfer_request_with_filter.rs"]
mod transfer_request_with_filter_tests;
#[path = "transfer_request_with_from0.rs"]
mod transfer_request_with_from0_tests;
#[path = "transfer_request_with_ignore.rs"]
mod transfer_request_with_ignore_tests;
#[path = "transfer_request_with_ignore_times.rs"]
mod transfer_request_with_ignore_times_tests;
#[path = "transfer_request_with_include.rs"]
mod transfer_request_with_include_tests;
#[path = "transfer_request_with_itemize.rs"]
mod transfer_request_with_itemize_tests;
#[path = "transfer_request_with_no.rs"]
mod transfer_request_with_no_tests;
#[path = "transfer_request_with_omit.rs"]
mod transfer_request_with_omit_tests;
#[path = "transfer_request_with_out.rs"]
mod transfer_request_with_out_tests;
#[path = "transfer_request_with_owner.rs"]
mod transfer_request_with_owner_tests;
#[path = "transfer_request_with_perms.rs"]
mod transfer_request_with_perms_tests;
#[path = "transfer_request_with_relative.rs"]
mod transfer_request_with_relative_tests;
#[path = "transfer_request_with_remove.rs"]
mod transfer_request_with_remove_tests;
#[path = "transfer_request_with_sparse.rs"]
mod transfer_request_with_sparse_tests;
#[path = "transfer_request_with_times.rs"]
mod transfer_request_with_times_tests;
#[path = "verbose.rs"]
mod verbose_tests;
#[path = "version.rs"]
mod version_tests;
#[path = "xattrs.rs"]
mod xattrs_tests;
