//! Receiver tests, decomposed by surface area.
//!
//! - [`support`] - shared fixtures and helpers reused across the surfaces.
//! - [`file_list`] - file-list receive, sanitize, incremental, sender-attrs,
//!   sum-head, ndx-convert, and the delete-pipeline hook.
//! - [`delta_apply`] - whole-file delta application, wire-to-script
//!   conversion, sparse writes, and checksum verifier coverage.
//! - [`hard_links`] - `create_hardlinks` behaviour and the
//!   `HardlinkApplyTracker` lifecycle.
//! - [`symlinks_and_devices`] - itemize emission for files, directories,
//!   symlinks, and other special entries.
//! - [`partial_resume`] - temp-file guard, relative-parent creation, and
//!   reference-directory lookups used during partial/resume transfers.
//! - [`errors_and_timeouts`] - error categorization, failed-directory
//!   propagation, legacy goodbye handling, input-multiplex activation,
//!   daemon filter set, and path-traversal rejection.

#[cfg(unix)]
mod create_specials;
mod delta_apply;
mod errors_and_timeouts;
mod file_list;
mod generator_keepalive;
mod hard_links;
#[cfg(unix)]
mod munge_symlinks;
mod parallel_delta_notice;
mod partial_resume;
mod post_decision_name_emission;
mod support;
mod symlinks_and_devices;
#[cfg(unix)]
mod verbose_dir_names;
#[cfg(windows)]
mod windows_receiver_symlinks;
#[cfg(windows)]
mod windows_special_skip;
