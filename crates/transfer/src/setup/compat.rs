//! Compatibility flags exchange.
//!
//! Handles writing and exchanging compatibility flags between server and client,
//! including the pre-release `'V'` capability encoding difference. Mirrors
//! upstream `compat.c:754-759`.

use super::capability::client_has_pre_release_v_flag;
use protocol::CompatibilityFlags;
use std::io::{self, Write};

/// Writes compatibility flags to the output stream, handling the pre-release
/// `'V'` capability flag encoding difference.
///
/// When the client advertises `'V'` (a deprecated pre-release flag), the
/// server writes the compat flags as a single byte and implicitly enables
/// `CF_VARINT_FLIST_FLAGS`. Otherwise, the flags are written using the
/// standard varint encoding.
///
/// The client-side `read_varint()` is compatible with both encodings because
/// a single byte with the high bit clear decodes identically under both
/// schemes.
///
/// # Upstream reference
///
/// `compat.c:745-750`:
/// ```c
/// if (strchr(client_info, 'V') != NULL) {
///     if (!write_batch)
///         compat_flags |= CF_VARINT_FLIST_FLAGS;
///     write_byte(f_out, compat_flags);
/// } else
///     write_varint(f_out, compat_flags);
/// ```
pub(crate) fn write_compat_flags<W: Write + ?Sized>(
    writer: &mut W,
    mut flags: CompatibilityFlags,
    client_info: &str,
) -> io::Result<CompatibilityFlags> {
    if client_has_pre_release_v_flag(client_info) {
        // Pre-release 'V' client: implicitly enable VARINT_FLIST_FLAGS and
        // write as a single byte (upstream: write_batch is never true here).
        // upstream: compat.c:755-757
        flags |= CompatibilityFlags::VARINT_FLIST_FLAGS;
        writer.write_all(&[flags.bits() as u8])?;
    } else {
        protocol::write_varint(writer, flags.bits() as i32)?;
    }
    Ok(flags)
}
