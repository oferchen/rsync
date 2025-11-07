#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `client` module exposes the orchestration entry points consumed by the
//! `rsync` CLI binary. The current implementation focuses on providing a
//! deterministic, synchronous local copy engine that mirrors the high-level
//! behaviour of `rsync SOURCE DEST` when no remote shells or daemons are
//! involved. The API models the configuration and error structures that higher
//! layers will reuse once network transports and the full delta-transfer engine
//! land.
//!
//! # Design
//!
//! - [`ClientConfig`](crate::client::ClientConfig) encapsulates the caller-provided
//!   transfer arguments. A
//!   builder is offered so future options (e.g. logging verbosity) can be wired
//!   through without breaking call sites. Today it exposes toggles for dry-run
//!   validation (`--dry-run`) and extraneous-destination cleanup (`--delete`).
//! - [`run_client`](crate::client::run_client) executes the client flow. The helper
//!   delegates to
//!   [`oc_rsync_engine::local_copy`] to mirror a simplified subset of upstream
//!   behaviour by copying files, directories, and symbolic links on the local
//!   filesystem while preserving permissions, timestamps, optional
//!   ownership/group metadata, and sparse regions when requested. Delta
//!   compression and advanced metadata such as ACLs or extended attributes
//!   remain out of scope for this snapshot. When remote operands are detected,
//!   the client delegates to the system `rsync` binary so network transfers are
//!   available while the native engine is completed. When
//!   deletion is requested (including [`--delete-excluded`](crate::client::ClientConfig::delete_excluded)),
//!   the helper removes destination entries that are absent from the source tree
//!   before applying metadata and prunes excluded entries when explicitly
//!   requested.
//! - [`ModuleListRequest`](crate::client::ModuleListRequest) parses
//!   daemon-style operands (`rsync://host/` or `host::`) and
//!   [`run_module_list`](crate::client::run_module_list) connects to the remote
//!   daemon using the legacy `@RSYNCD:` negotiation to retrieve the advertised
//!   module table.
//! - [`ClientError`](crate::client::ClientError) carries the exit code and fully
//!   formatted [`crate::message::Message`] so binaries can surface diagnostics
//!   via the central rendering helpers.
//!
//! # Invariants
//!
//! - `ClientError::exit_code` always matches the exit code embedded in the
//!   [`crate::message::Message`].
//! - `run_client` never panics and preserves the provided configuration even
//!   when reporting unsupported functionality.
//!
//! # Errors
//!
//! All failures are routed through [`ClientError`](crate::client::ClientError).
//! The structure implements [`std::error::Error`], allowing integration with
//! higher-level error handling stacks without losing access to the formatted
//! diagnostic.
//!
//! # Examples
//!
//! Running the client with a single source copies the file into the destination
//! path. The helper currently operates entirely on the local filesystem.
//!
//! ```
//! use oc_rsync_core::client::{run_client, ClientConfig};
//! use std::fs;
//! use tempfile::tempdir;
//!
//! let temp = tempdir().unwrap();
//! let source = temp.path().join("source.txt");
//! let destination = temp.path().join("dest.txt");
//! fs::write(&source, b"example").unwrap();
//!
//! let config = ClientConfig::builder()
//!     .transfer_args([source.clone(), destination.clone()])
//!     .build();
//!
//! let summary = run_client(config).expect("local copy succeeds");
//! assert_eq!(summary.files_copied(), 1);
//! assert_eq!(fs::read(&destination).unwrap(), b"example");
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the formatting utilities reused by the client
//!   orchestration.
//! - [`crate::version`] for the canonical version banner shared with the CLI.
//! - [`oc_rsync_engine::local_copy`] for the transfer plan executed by this module.

mod config;
mod error;
mod fallback;
mod module_list;
mod outcome;
mod progress;
mod run;
mod summary;

pub use self::config::{
    AddressMode, BandwidthLimit, BindAddress, ClientConfig, ClientConfigBuilder,
    CompressionSetting, DeleteMode, FilterRuleKind, FilterRuleSpec, HumanReadableMode,
    HumanReadableModeParseError, IconvParseError, IconvSetting, ReferenceDirectory,
    ReferenceDirectoryKind, StrongChecksumAlgorithm, StrongChecksumChoice, TransferTimeout,
    force_no_compress_from_env, parse_skip_compress_list, skip_compress_from_env,
};
pub use self::error::{
    ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, PARTIAL_TRANSFER_EXIT_CODE, SOCKET_IO_EXIT_CODE,
};
pub use self::fallback::{RemoteFallbackArgs, RemoteFallbackContext, run_remote_transfer_fallback};
pub use self::module_list::{
    DaemonAddress, ModuleList, ModuleListEntry, ModuleListOptions, ModuleListRequest,
    run_module_list, run_module_list_with_options, run_module_list_with_password,
    run_module_list_with_password_and_options,
};
pub use self::outcome::{ClientOutcome, FallbackSummary};
pub use self::progress::{ClientProgressObserver, ClientProgressUpdate};
pub use self::run::{run_client, run_client_or_fallback, run_client_with_observer};
pub use self::summary::{
    ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind, ClientSummary,
};
pub use oc_rsync_engine::SkipCompressList;
pub use oc_rsync_engine::local_copy::{DirMergeEnforcedKind, DirMergeOptions};

use std::time::Duration;

#[allow(unused_imports)]
pub(crate) use self::error::{
    MAX_DELETE_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE, daemon_access_denied_error,
    daemon_authentication_failed_error, daemon_authentication_required_error, daemon_error,
    daemon_listing_unavailable_error, daemon_protocol_error, socket_error,
};
pub(crate) const DAEMON_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const MAX_EXIT_CODE: i32 = u8::MAX as i32;

#[cfg(test)]
mod tests;
