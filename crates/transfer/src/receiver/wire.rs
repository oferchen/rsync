//! Wire protocol types for the receiver role.
//!
//! Defines the signature header (`SumHead`), sender response attributes
//! (`SenderAttrs`), and the signature block writer used during the
//! request/response exchange between receiver and sender.

use std::io::{self, Read, Write};

use engine::signature::FileSignature;
use protocol::codec::NdxCodec;
use protocol::read_varint;
use protocol::xattr::{MAX_WIRE_XATTR_VALUE_LEN, XattrList};

/// Upstream MAXPATHLEN ceiling for an xname vstring (io.c:1944-1960).
const MAX_XNAME_LEN: usize = 4096;

/// Validates and wraps a sender-echoed basis-type byte.
///
/// Used by [`SenderAttrs::read_with_codec_xattr`] to decode the `fnamecmp_type`.
/// Upstream `rsync.c:read_ndx_and_attrs` reads a single byte and maps it through
/// `FnameCmpType`.
fn parse_fnamecmp_type(byte: u8) -> io::Result<protocol::FnameCmpType> {
    protocol::FnameCmpType::from_wire(byte).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid fnamecmp type: 0x{:02X} {}{}",
                byte,
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            ),
        )
    })
}

/// Decodes an xname vstring length from its 1- or 2-byte prefix.
///
/// If the high bit of `len_byte` is set the length spans two bytes
/// (`(len_byte & 0x7F) * 256 + second`); otherwise it is `len_byte`. Upstream
/// `io.c:1944-1960` `read_vstring()`.
#[inline]
fn xname_len_from_bytes(len_byte: u8, second: Option<u8>) -> usize {
    if len_byte & 0x80 != 0 {
        ((len_byte & 0x7F) as usize) * 256 + second.unwrap_or(0) as usize
    } else {
        len_byte as usize
    }
}

/// Rejects an oversized xname length with a typed error.
#[inline]
fn check_xname_len(xname_len: usize) -> io::Result<()> {
    if xname_len > MAX_XNAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "xname length {xname_len} exceeds maximum {MAX_XNAME_LEN} {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            ),
        ));
    }
    Ok(())
}

/// Decides whether the sender-response frame carries xattr abbreviation data.
///
/// Mirrors upstream `receiver.c:609-611`: read when xattrs are preserved and
/// `ITEM_REPORT_XATTR` is set, unless the xattr hardlink optimization elides it
/// for a local-change rename.
#[inline]
fn want_xattr_read(preserve_xattrs: bool, iflags: u16, want_xattr_optim: bool) -> bool {
    preserve_xattrs
        && (iflags & SenderAttrs::ITEM_REPORT_XATTR != 0)
        && !(want_xattr_optim
            && (iflags & SenderAttrs::ITEM_XNAME_FOLLOWS != 0)
            && (iflags & SenderAttrs::ITEM_LOCAL_CHANGE != 0))
}

/// Signature header for delta transfer.
///
/// Represents the `sum_head` structure from upstream rsync's rsync.h.
/// Contains metadata about the signature blocks that follow.
///
/// # Wire Format
///
/// All fields are transmitted as 32-bit little-endian integers:
/// - `count`: Number of signature blocks
/// - `blength`: Block length in bytes
/// - `s2length`: Strong sum (checksum) length in bytes
/// - `remainder`: Size of the final partial block (0 if file is block-aligned)
///
/// # Upstream Reference
///
/// - `rsync.h:200` - `struct sum_struct` definition
/// - `match.c:380` - `write_sum_head()` sends the header
/// - `sender.c:120` - `read_sum_head()` receives the header
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SumHead {
    /// Number of signature blocks.
    pub count: u32,
    /// Block length in bytes.
    pub blength: u32,
    /// Strong sum (checksum) length in bytes.
    pub s2length: u32,
    /// Size of the final partial block (0 if block-aligned).
    pub remainder: u32,
}

impl SumHead {
    /// Creates a new `SumHead` with the specified parameters.
    #[must_use]
    pub const fn new(count: u32, blength: u32, s2length: u32, remainder: u32) -> Self {
        Self {
            count,
            blength,
            s2length,
            remainder,
        }
    }

    /// Creates an empty `SumHead` (no basis file, requests whole-file transfer).
    ///
    /// When count=0, the sender knows to transmit the entire file as literal data.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            count: 0,
            blength: 0,
            s2length: 0,
            remainder: 0,
        }
    }

    /// Computes the total file length described by this sum_head.
    ///
    /// Used in append mode to determine the offset at which to start writing
    /// new data (the sender only sends bytes beyond this length).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:289-291` - `sum.flength = count * blength; if (remainder) flength -= blength - remainder`
    #[must_use]
    pub const fn flength(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let total = self.count as u64 * self.blength as u64;
        if self.remainder > 0 {
            total - self.blength as u64 + self.remainder as u64
        } else {
            total
        }
    }

    /// Creates a `SumHead` from a file signature.
    #[must_use]
    pub const fn from_signature(signature: &FileSignature) -> Self {
        let layout = signature.layout();
        Self {
            count: layout.block_count() as u32,
            blength: layout.block_length().get(),
            s2length: layout.strong_sum_length().get() as u32,
            remainder: layout.remainder(),
        }
    }

    /// Returns true if this represents a whole-file transfer (no basis).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Writes the sum_head to the wire in rsync format.
    ///
    /// All four fields are written as 32-bit little-endian integers.
    pub fn write<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&(self.count as i32).to_le_bytes())?;
        writer.write_all(&(self.blength as i32).to_le_bytes())?;
        writer.write_all(&(self.s2length as i32).to_le_bytes())?;
        writer.write_all(&(self.remainder as i32).to_le_bytes())?;
        Ok(())
    }

    /// Decodes and validates a sum_head from its 16-byte little-endian wire
    /// representation.
    ///
    /// The four fields arrive from an authenticated but untrusted peer and size
    /// downstream allocations (`Vec::with_capacity(count)`,
    /// `vec![0u8; s2length]`, per-block `vec![0u8; s2length]`). Without bounds a
    /// malicious sum_head is a memory-exhaustion (DoS) vector, so every field is
    /// range-checked exactly as upstream does before it is used.
    ///
    /// Rejections return an
    /// [`io::ErrorKind::InvalidData`] error, which the receiver already maps to
    /// the `RERR_PROTOCOL` (exit code 2) path for malformed wire input.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:2025-2067` - `read_sum_head()` validates every field and calls
    ///   `exit_cleanup(RERR_PROTOCOL)` on any out-of-range value.
    fn from_wire_bytes(buf: &[u8; 16]) -> io::Result<Self> {
        // Fields are signed on the wire (write_int/read_int); decode as i32 so
        // the negative-value guards below mirror upstream's `< 0` checks.
        let count = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let blength = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let s2length = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let remainder = i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);

        // upstream: io.c:2029 - reject negative block count.
        if count < 0 {
            return Err(Self::malformed(format!("invalid checksum count {count}")));
        }
        // upstream: io.c:2039-2048 - guard against overflow in the downstream
        // `count * per_entry_size` allocation arithmetic. Each signature block
        // occupies `4 + s2length` wire bytes; MAX_STRONG_SUM_LEN bounds the
        // second factor so the product cannot overflow usize.
        let per_entry = 4usize + Self::MAX_STRONG_SUM_LEN;
        if (count as usize).checked_mul(per_entry).is_none() {
            return Err(Self::malformed(format!(
                "invalid checksum count {count} (allocation overflow)"
            )));
        }
        // upstream: io.c:2050-2054 - blength in (0, max_blength]. We use the
        // legacy MAX_BLOCK_SIZE (1<<29) as the permissive ceiling so any header
        // accepted by either protocol era is accepted here; a zero/negative
        // block length is nonsense (division-by-zero) when count > 0.
        if blength < 0
            || blength as u32 > signature::MAX_BLOCK_SIZE_OLD
            || (count > 0 && blength == 0)
        {
            return Err(Self::malformed(format!("invalid block length {blength}")));
        }
        // upstream: io.c:2056-2060 - s2length in [0, xfer_sum_len]. oc's longest
        // negotiable transfer digest is SHA1 at MAX_STRONG_SUM_LEN (20) bytes.
        if s2length < 0 || s2length as usize > Self::MAX_STRONG_SUM_LEN {
            return Err(Self::malformed(format!(
                "invalid checksum length {s2length}"
            )));
        }
        // upstream: io.c:2061-2066 - remainder in [0, blength].
        if remainder < 0 || remainder > blength {
            return Err(Self::malformed(format!(
                "invalid remainder length {remainder}"
            )));
        }

        Ok(Self {
            count: count as u32,
            blength: blength as u32,
            s2length: s2length as u32,
            remainder: remainder as u32,
        })
    }

    /// Maximum strong-sum length accepted in a sum_head, in bytes.
    ///
    /// Mirrors upstream's `xfer_sum_len` ceiling (`io.c:2056`): the length of
    /// the longest transfer checksum oc can negotiate. SHA1 (20 bytes) is the
    /// longest supported transfer digest, so s2length may not exceed 20.
    const MAX_STRONG_SUM_LEN: usize = 20;

    /// Builds a `RERR_PROTOCOL`-mapped error for a malformed sum_head field.
    ///
    /// upstream: io.c:2032-2065 `read_sum_head()` validates every field and
    /// calls `exit_cleanup(RERR_PROTOCOL)` (exit 2) on any out-of-range value.
    /// The error is tagged so the core exit-code mapper yields RERR_PROTOCOL(2)
    /// rather than RERR_STREAMIO(12).
    fn malformed(detail: String) -> io::Error {
        protocol::protocol_violation(format!(
            "malformed sum_head: {detail} {}{}",
            crate::role_trailer::error_location!(),
            crate::role_trailer::receiver()
        ))
    }

    /// Reads a sum_head from the wire in rsync format.
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;
        Self::from_wire_bytes(&buf)
    }
}

/// Attributes echoed back by the sender after receiving a file request.
///
/// After the receiver sends NDX + iflags + sum_head, the sender echoes back
/// its own NDX + iflags, potentially with additional fields based on flags.
/// When `ITEM_REPORT_XATTR` is set, xattr abbreviation data follows the
/// standard fields.
///
/// # Upstream Reference
///
/// - `sender.c:180` - `write_ndx_and_attrs()` sends these
/// - `rsync.c:383` - `read_ndx_and_attrs()` reads them
/// - `xattrs.c:623` - `send_xattr_request()` writes abbreviated xattr values
#[derive(Debug, Clone, Default)]
pub struct SenderAttrs {
    /// Item flags indicating transfer mode.
    pub iflags: u16,
    /// Optional basis file type (if `ITEM_BASIS_TYPE_FOLLOWS` set).
    ///
    /// When present, indicates which basis file the generator selected for
    /// the delta transfer. See `FnameCmpType` for the possible values.
    pub fnamecmp_type: Option<protocol::FnameCmpType>,
    /// Optional alternate basis name (if `ITEM_XNAME_FOLLOWS` set).
    pub xname: Option<Vec<u8>>,
    /// Abbreviated xattr values received from the sender.
    ///
    /// When `ITEM_REPORT_XATTR` is set in iflags, the sender transmits full
    /// values for previously abbreviated xattr entries. Each entry is a
    /// (1-based entry num, value) pair. The receiver uses these to replace
    /// checksum-only entries in the xattr cache.
    ///
    /// # Upstream Reference
    ///
    /// - `xattrs.c:681` - `recv_xattr_request()` reads these on the receiver side
    pub xattr_values: Vec<(i32, Vec<u8>)>,
}

impl SenderAttrs {
    /// Item flag indicating file data will be transferred.
    pub const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
    /// Item flag indicating basis type follows.
    pub const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11; // 0x0800
    /// Item flag indicating alternate name follows.
    pub const ITEM_XNAME_FOLLOWS: u16 = 1 << 12; // 0x1000
    /// Item flag indicating xattr data follows.
    pub const ITEM_REPORT_XATTR: u16 = 1 << 8; // 0x0100
    /// Item flag indicating local change (e.g., hardlink with no transfer).
    pub const ITEM_LOCAL_CHANGE: u16 = 1 << 14; // 0x4000

    /// Reads sender attributes from the wire using an NDX codec.
    ///
    /// The sender echoes back NDX + iflags after receiving a file request.
    /// Protocol 30+ uses variable-length delta-encoded NDX values, which
    /// require the codec to maintain state across calls.
    ///
    /// When `preserve_xattrs` is true and the sender sets `ITEM_REPORT_XATTR`,
    /// this also reads abbreviated xattr values from the wire. The
    /// `want_xattr_optim` flag mirrors upstream's `CF_AVOID_XATTR_OPTIM`
    /// capability - when active, xattr exchange is skipped for local-change
    /// hardlinks.
    ///
    /// # Arguments
    ///
    /// * `reader` - The input stream to read from
    /// * `ndx_codec` - The NDX codec for protocol-aware decoding
    /// * `preserve_xattrs` - Whether xattr preservation is active
    /// * `want_xattr_optim` - Whether xattr hardlink optimization is active
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:2289-2318` - `read_ndx()` for NDX decoding
    /// - `rsync.c:383` - `read_ndx_and_attrs()` reads NDX + iflags
    /// - `receiver.c:609-611` - receiver reads xattr data when ITEM_REPORT_XATTR set
    /// - `xattrs.c:681` - `recv_xattr_request()` reads abbreviated values
    pub fn read_with_codec<R: Read>(
        reader: &mut R,
        ndx_codec: &mut impl NdxCodec,
    ) -> io::Result<(i32, Self)> {
        Self::read_with_codec_xattr(reader, ndx_codec, false, false)
    }

    /// Reads sender attributes with xattr support.
    ///
    /// See [`read_with_codec`](Self::read_with_codec) for basic usage. This
    /// variant adds xattr abbreviation data reading when the sender includes
    /// `ITEM_REPORT_XATTR` in iflags.
    pub fn read_with_codec_xattr<R: Read>(
        reader: &mut R,
        ndx_codec: &mut impl NdxCodec,
        preserve_xattrs: bool,
        want_xattr_optim: bool,
    ) -> io::Result<(i32, Self)> {
        // Read NDX using protocol-aware codec (handles delta encoding for protocol 30+)
        let ndx = ndx_codec.read_ndx(reader)?;

        let protocol_version = ndx_codec.protocol_version();

        // For protocol >= 29, read iflags (shortint = 2 bytes LE)
        let iflags = if protocol_version >= 29 {
            let mut iflags_buf = [0u8; 2];
            reader.read_exact(&mut iflags_buf)?;
            u16::from_le_bytes(iflags_buf)
        } else {
            Self::ITEM_TRANSFER // Default for older protocols
        };

        // Read optional fields based on iflags
        let fnamecmp_type = if iflags & Self::ITEM_BASIS_TYPE_FOLLOWS != 0 {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(parse_fnamecmp_type(byte[0])?)
        } else {
            None
        };

        let xname = if iflags & Self::ITEM_XNAME_FOLLOWS != 0 {
            // Read vstring: upstream io.c:1944-1960 read_vstring()
            // Format: first byte is length; if bit 7 set, length = (byte & 0x7F) * 256 + next_byte
            let mut len_byte = [0u8; 1];
            reader.read_exact(&mut len_byte)?;
            let xname_len = if len_byte[0] & 0x80 != 0 {
                let mut second_byte = [0u8; 1];
                reader.read_exact(&mut second_byte)?;
                xname_len_from_bytes(len_byte[0], Some(second_byte[0]))
            } else {
                xname_len_from_bytes(len_byte[0], None)
            };
            check_xname_len(xname_len)?;
            if xname_len > 0 {
                let mut xname_buf = vec![0u8; xname_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        // upstream: receiver.c:609-611 - read xattr data when ITEM_REPORT_XATTR is set
        // Condition mirrors upstream: preserve_xattrs && iflags & ITEM_REPORT_XATTR && do_xfers
        // && !(want_xattr_optim && BITS_SET(iflags, ITEM_XNAME_FOLLOWS|ITEM_LOCAL_CHANGE))
        let xattr_values = if want_xattr_read(preserve_xattrs, iflags, want_xattr_optim) {
            read_xattr_abbreviation_data(reader)?
        } else {
            Vec::new()
        };

        Ok((
            ndx,
            Self {
                iflags,
                fnamecmp_type,
                xname,
                xattr_values,
            },
        ))
    }

    /// Reads sender attributes from the wire (legacy method for tests).
    ///
    /// **Deprecated**: Use [`read_with_codec`](Self::read_with_codec) for proper protocol 30+ support.
    /// This method only reads a single byte for NDX, which is incorrect for
    /// protocol 30+ that uses variable-length delta encoding.
    ///
    /// # Arguments
    ///
    /// * `reader` - The input stream to read from
    /// * `protocol_version` - The negotiated protocol version
    #[cfg(test)]
    pub fn read<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Self> {
        // Legacy implementation: read single byte for NDX (only valid for tests
        // with protocol < 30 or first NDX where delta=1 fits in one byte)
        let mut ndx_byte = [0u8; 1];
        let n = reader.read(&mut ndx_byte)?;
        if n != 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to read NDX byte from sender",
            ));
        }

        // For protocol >= 29, read iflags (shortint = 2 bytes LE)
        let iflags = if protocol_version >= 29 {
            let mut iflags_buf = [0u8; 2];
            reader.read_exact(&mut iflags_buf)?;
            u16::from_le_bytes(iflags_buf)
        } else {
            Self::ITEM_TRANSFER // Default for older protocols
        };

        // Read optional fields based on iflags
        let fnamecmp_type = if iflags & Self::ITEM_BASIS_TYPE_FOLLOWS != 0 {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(protocol::FnameCmpType::from_wire(byte[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid fnamecmp type: 0x{:02X}", byte[0]),
                )
            })?)
        } else {
            None
        };

        let xname = if iflags & Self::ITEM_XNAME_FOLLOWS != 0 {
            let mut len_byte = [0u8; 1];
            reader.read_exact(&mut len_byte)?;
            let xname_len = if len_byte[0] & 0x80 != 0 {
                let mut second_byte = [0u8; 1];
                reader.read_exact(&mut second_byte)?;
                ((len_byte[0] & 0x7F) as usize) * 256 + second_byte[0] as usize
            } else {
                len_byte[0] as usize
            };
            const MAX_XNAME_LEN: usize = 4096;
            if xname_len > MAX_XNAME_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("xname length {xname_len} exceeds maximum {MAX_XNAME_LEN}"),
                ));
            }
            if xname_len > 0 {
                let mut xname_buf = vec![0u8; xname_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            iflags,
            fnamecmp_type,
            xname,
            xattr_values: Vec::new(),
        })
    }
}

/// Writes signature blocks to the wire.
///
/// After writing sum_head, this sends each block's rolling sum and strong sum.
///
/// # Upstream Reference
///
/// - `match.c:395` - Signature block transmission
pub fn write_signature_blocks<W: Write + ?Sized>(
    writer: &mut W,
    signature: &FileSignature,
    s2length: u32,
) -> io::Result<()> {
    let mut sum_buf = vec![0u8; s2length as usize];
    for block in signature.blocks() {
        // Write rolling_sum as int32 LE
        writer.write_all(&(block.rolling().value() as i32).to_le_bytes())?;

        // Write strong_sum, truncated or padded to s2length
        let strong_bytes = block.strong();
        sum_buf.fill(0);
        let copy_len = std::cmp::min(strong_bytes.len(), s2length as usize);
        sum_buf[..copy_len].copy_from_slice(&strong_bytes[..copy_len]);
        writer.write_all(&sum_buf)?;
    }
    Ok(())
}

/// Reads abbreviated xattr values from the sender.
///
/// The sender transmits 1-based entry numbers (delta-encoded) followed by
/// full value data for each entry. A zero terminates the list.
///
/// Returns `(num, value)` pairs where `num` is the 1-based entry number
/// from the original xattr list.
///
/// # Wire Format
///
/// ```text
/// For each entry:
///   relative_num : varint  // num - prior_req (1-based, delta-encoded)
///   length       : varint  // value byte count
///   value        : bytes[length]
/// terminator     : varint  // 0 signals end of list
/// ```
///
/// # Upstream Reference
///
/// - `xattrs.c:681-757` - `recv_xattr_request()` called by receiver (!am_sender)
fn read_xattr_abbreviation_data<R: Read>(reader: &mut R) -> io::Result<Vec<(i32, Vec<u8>)>> {
    let mut values = Vec::new();
    let mut prior_req = 0i32;

    loop {
        let rel_pos = read_varint(reader)?;
        if rel_pos == 0 {
            break;
        }
        let num = accumulate_xattr_num(prior_req, rel_pos)?;
        prior_req = num;

        let raw_len = read_varint(reader)?;
        let datum_len = check_xattr_datum_len(raw_len)?;
        let mut value = vec![0u8; datum_len];
        reader.read_exact(&mut value)?;

        values.push((num, value));
    }

    Ok(values)
}

/// Accumulates the 1-based xattr entry number, rejecting signed overflow.
///
/// Upstream `xattrs.c:700-705`
/// rejects overflow before `num += rel_pos` to stop a hostile peer wrapping
/// `num` to an arbitrary value.
#[inline]
fn accumulate_xattr_num(prior_req: i32, rel_pos: i32) -> io::Result<i32> {
    prior_req.checked_add(rel_pos).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "xattr rel_pos accumulation overflow {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            ),
        )
    })
}

/// Validates a raw xattr datum length against the receiver-side ceiling.
///
/// Upstream `xattrs.c:752` reads
/// `datum_len` via `read_varint_size(..., MAX_WIRE_XATTR_DATALEN, ...)`; we use
/// the oc-rsync ceiling (`MAX_WIRE_XATTR_VALUE_LEN`, upstream default --max-alloc) so a
/// corrupt or hostile frame surfaces as a typed error instead of an unbounded
/// allocation or a varint overflow panic.
#[inline]
fn check_xattr_datum_len(raw_len: i32) -> io::Result<usize> {
    if raw_len < 0 || raw_len as usize > MAX_WIRE_XATTR_VALUE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "xattr datum_len {raw_len} exceeds maximum {MAX_WIRE_XATTR_VALUE_LEN} {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            ),
        ));
    }
    Ok(raw_len as usize)
}

/// Writes the generator-side xattr abbreviation request.
///
/// The generator sends 1-based entry numbers (delta-encoded) for the
/// abbreviated xattr values it needs. No value data is included - only the
/// entry numbers. A zero terminates the list.
///
/// # Wire Format
///
/// ```text
/// For each needed entry:
///   relative_num : varint  // num - prior_req (1-based, delta-encoded)
/// terminator     : varint  // 0 signals end of list
/// ```
///
/// # Upstream Reference
///
/// - `xattrs.c:623-675` - `send_xattr_request()` called by generator (fname=NULL)
pub fn write_xattr_request<W: Write + ?Sized>(
    writer: &mut W,
    xattr_list: &XattrList,
) -> io::Result<()> {
    use protocol::write_varint;
    use protocol::xattr::{MAX_FULL_DATUM, XattrState};

    let mut prior_req = 0i32;

    for entry in xattr_list.iter() {
        if entry.datum_len() <= MAX_FULL_DATUM {
            continue;
        }
        match entry.state() {
            // upstream: XSTATE_ABBREV entries matched sender's checksum - skip
            XattrState::Abbrev => continue,
            // upstream: XSTATE_TODO entries need full data from sender
            XattrState::Todo => {}
            // upstream: default - skip
            XattrState::Done => continue,
        }

        let num = entry.num() as i32;
        write_varint(writer, num - prior_req)?;
        prior_req = num;
    }

    // upstream: write_byte(f_out, 0) - terminate the request list
    write_varint(writer, 0)?;

    Ok(())
}

/// Applies received abbreviated xattr values to the xattr cache.
///
/// After the receiver reads abbreviated xattr values from the sender via
/// `read_xattr_abbreviation_data`, this function updates the corresponding
/// entries in the xattr cache with full values.
///
/// # Arguments
///
/// * `xattr_list` - The xattr list to update (from the xattr cache)
/// * `values` - The (1-based num, value) pairs from the sender
///
/// # Upstream Reference
///
/// - `xattrs.c:744-755` - receiver stores full values in place of checksums
pub fn apply_xattr_abbreviation_values(xattr_list: &mut XattrList, values: &[(i32, Vec<u8>)]) {
    for (num, value) in values {
        // Find the entry by its 1-based num. Upstream scans forward with wrap.
        if let Some(entry) = xattr_list
            .entries_mut()
            .iter_mut()
            .find(|e| e.num() as i32 == *num)
        {
            entry.set_full_value(value.clone());
        }
    }
}

#[cfg(test)]
mod xattr_abbrev_guard_tests {
    //! Defensive-bounds regression tests for `read_xattr_abbreviation_data`.
    //!
    //! These cover the upstream hardening at `xattrs.c:700-705` (signed
    //! rel_pos overflow) and `xattrs.c:752` (bounded `datum_len`). When the
    //! sender wire is misaligned or an attacker injects a hostile frame at
    //! the xattr-abbreviation offset, we want a typed `InvalidData` error
    //! instead of an unbounded allocation or a varint overflow panic.
    use std::io::Write;
    use std::io::{Cursor, ErrorKind};

    use protocol::write_varint;

    use super::read_xattr_abbreviation_data;

    fn encode_varint(value: i32) -> Vec<u8> {
        let mut buf = Vec::new();
        write_varint(&mut buf, value).expect("write_varint");
        buf
    }

    #[test]
    fn rel_pos_overflow_returns_typed_error() {
        // First rel_pos lifts num to near i32::MAX with a zero-length value,
        // then the second rel_pos would overflow the signed accumulator.
        // Mirrors upstream xattrs.c:700-705.
        let mut bytes = Vec::new();
        bytes.extend(encode_varint(i32::MAX - 1));
        bytes.extend(encode_varint(0));
        bytes.extend(encode_varint(2));

        let mut reader = Cursor::new(bytes);
        let err = read_xattr_abbreviation_data(&mut reader).expect_err("must error");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("accumulation overflow"));
    }

    #[test]
    fn malformed_sum_head_is_tagged_protocol_violation() {
        // WHY: upstream io.c:2032 read_sum_head() aborts a negative checksum
        // count with exit_cleanup(RERR_PROTOCOL) (exit 2), not RERR_STREAMIO.
        // The InvalidData error must carry the ProtocolViolation marker so the
        // core exit-code mapper yields 2; otherwise a hostile sum_head collapses
        // into the streamio(12) bucket.
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&(-1i32).to_le_bytes());
        let mut reader = Cursor::new(buf.to_vec());
        let err = super::SumHead::read(&mut reader).expect_err("negative count must abort");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(
            err.get_ref()
                .is_some_and(|e| e.is::<protocol::ProtocolViolation>()),
            "malformed sum_head must be tagged RERR_PROTOCOL"
        );
        assert!(err.to_string().contains("malformed sum_head"));
    }

    #[test]
    fn datum_len_exceeding_cap_returns_typed_error() {
        // datum_len just above the receiver-side ceiling must be
        // rejected with InvalidData rather than triggering a giant `vec!`
        // allocation. Mirrors upstream xattrs.c:752 bounded read.
        let mut bytes = Vec::new();
        bytes.extend(encode_varint(1));
        bytes.extend(encode_varint(
            (protocol::xattr::MAX_WIRE_XATTR_VALUE_LEN as i32) + 1,
        ));

        let mut reader = Cursor::new(bytes);
        let err = read_xattr_abbreviation_data(&mut reader).expect_err("must error");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("datum_len"));
    }

    #[test]
    fn negative_datum_len_returns_typed_error() {
        // A negative varint at the datum_len slot would historically widen
        // into a giant `usize` via `as usize`. Now it returns InvalidData.
        let mut bytes = Vec::new();
        bytes.extend(encode_varint(1));
        bytes.extend(encode_varint(-1));

        let mut reader = Cursor::new(bytes);
        let err = read_xattr_abbreviation_data(&mut reader).expect_err("must error");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("datum_len"));
    }

    #[test]
    fn empty_list_terminator_succeeds() {
        // Positive control: a lone zero terminator yields an empty Vec.
        let mut bytes = Vec::new();
        bytes.extend(encode_varint(0));

        let mut reader = Cursor::new(bytes);
        let values = read_xattr_abbreviation_data(&mut reader).expect("ok");
        assert!(values.is_empty());
    }

    #[test]
    fn single_legal_entry_round_trips() {
        // Positive control: one (num, value) pair with a small datum_len
        // succeeds and parses correctly.
        let mut bytes = Vec::new();
        bytes.extend(encode_varint(3));
        bytes.extend(encode_varint(4));
        bytes.write_all(b"abcd").unwrap();
        bytes.extend(encode_varint(0));

        let mut reader = Cursor::new(bytes);
        let values = read_xattr_abbreviation_data(&mut reader).expect("ok");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].0, 3);
        assert_eq!(values[0].1, b"abcd");
    }
}
