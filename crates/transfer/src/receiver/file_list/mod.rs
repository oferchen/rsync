//! File list reception and incremental processing.
//!
//! Handles receiving the file list from the sender, incremental sub-list
//! reception for INC_RECURSE mode, and the streaming
//! [`IncrementalFileListReceiver`] interface.
//!
//! Split into focused submodules to keep each file within the 650-line cap:
//!
//! - `receive` - `receive_file_list`, `receive_extra_file_lists`,
//!   `publish_segment_to_delete_pipeline`, and `incremental_file_list_receiver`.
//! - `id_lists` - UID/GID name-to-ID mapping reception.
//! - `sanitize` - file-list sanity validation against directory traversal.
//! - `hardlinks` - post-sort hardlink leader/follower assignment for
//!   protocol 30+ and pre-30 normalization from (dev, ino) pairs.
//! - `incremental` - the streaming [`IncrementalFileListReceiver`] type.

mod hardlinks;
mod id_lists;
mod incremental;
mod prune;
mod receive;
#[cfg(feature = "tokio-transfer")]
mod receive_async;
mod sanitize;

pub use incremental::IncrementalFileListReceiver;
