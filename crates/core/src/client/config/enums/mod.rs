//! Configuration enums for client transfer options.
//!
//! Each submodule owns a single logical concern - timeout policy, human-readable
//! output formatting, checksum algorithm selection, address family preference,
//! compression level, file-list source, and deletion scheduling.

mod address;
mod checksum;
mod compression;
mod delete;
mod files_from;
mod human_readable;
mod tcp_fastopen;
mod timeout;

pub use address::AddressMode;
pub use checksum::{StrongChecksumAlgorithm, StrongChecksumChoice};
pub use compression::CompressionSetting;
pub use delete::DeleteMode;
pub use files_from::{FilesFromPlan, FilesFromSource};
pub use human_readable::{HumanReadableMode, HumanReadableModeParseError};
pub use tcp_fastopen::{ParseTcpFastOpenModeError, TcpFastOpenMode};
pub use timeout::TransferTimeout;
