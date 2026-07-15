//! File list reception and incremental processing.
//!
//! Handles receiving the file list from the sender, incremental sub-list
//! reception for INC_RECURSE mode, and the streaming
//! [`IncrementalFileListReceiver`] interface.
//!
//! Split into focused submodules to keep each file within the 650-line cap:
//!
//! - `receive` - `receive_file_list`, `receive_extra_file_lists`,
//!   `receive_one_extra_segment`, `publish_segment_to_delete_pipeline`, and
//!   `incremental_file_list_receiver`.
//! - `on_demand` - lazy INC_RECURSE segment fetch (`read_next_frame`,
//!   `ensure_flat_idx`, `ensure_all_segments_loaded`, `prefetch_for_hardlinks`).
//! - `id_lists` - UID/GID name-to-ID mapping reception.
//! - `sanitize` - file-list sanity validation against directory traversal.
//! - `filter_recheck` - receiver-side re-filtering of received names against
//!   the receiver's own filter chain (upstream `recv_file_entry()`
//!   `check_server_filter`).
//! - `hardlinks` - post-sort hardlink leader/follower assignment for
//!   protocol 30+ and pre-30 normalization from (dev, ino) pairs.
//! - `incremental` - the streaming [`IncrementalFileListReceiver`] type.

mod filter_recheck;
mod hardlinks;
mod id_lists;
mod incremental;
mod on_demand;
mod prune;
mod receive;
#[cfg(feature = "tokio-transfer")]
mod receive_async;
mod sanitize;

pub use incremental::IncrementalFileListReceiver;
