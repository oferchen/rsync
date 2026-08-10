//! NDX wire protocol constants.
//!
//! These constants define sentinel values used in rsync's file-list index
//! protocol. They match upstream `rsync.h:285-288` definitions exactly.

/// NDX_DONE value indicating end of file requests.
///
/// Upstream: `rsync.h:285` - `#define NDX_DONE -1`
pub const NDX_DONE: i32 = -1;

/// NDX_FLIST_EOF value indicating end of file list(s).
///
/// Sent after the last incremental file list to signal no more file lists.
///
/// Upstream: `rsync.h:286` - `#define NDX_FLIST_EOF -2`
pub const NDX_FLIST_EOF: i32 = -2;

/// NDX_DEL_STATS value for delete statistics.
///
/// Upstream: `rsync.h:287` - `#define NDX_DEL_STATS -3`
pub const NDX_DEL_STATS: i32 = -3;

/// Offset for incremental file list directory indices.
///
/// Upstream: `rsync.h:288` - `#define NDX_FLIST_OFFSET -101`
pub const NDX_FLIST_OFFSET: i32 = -101;

/// NDX_DONE as 4-byte little-endian wire bytes for protocol < 30.
///
/// This is the raw representation of -1 as a signed 32-bit little-endian integer,
/// matching upstream's `write_int()` / `read_int()` for the goodbye handshake
/// in protocol versions 28 and 29.
///
/// # Upstream Reference
///
/// - `io.c` - `write_int()` / `read_int()` used by `main.c:883` for protocol < 29
/// - `io.c:2243-2287` - `write_ndx()` (protocol >= 30 uses varint instead)
pub const NDX_DONE_LEGACY_BYTES: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// NDX_DONE as modern varint wire byte for protocol >= 30.
///
/// The modern NDX codec encodes NDX_DONE (-1) as a single zero byte, distinct
/// from the legacy 4-byte encoding.
///
/// # Upstream Reference
///
/// - `io.c:2259-2262` - `write_ndx()` encodes NDX_DONE as `0x00`
pub const NDX_DONE_MODERN_BYTE: u8 = 0x00;
