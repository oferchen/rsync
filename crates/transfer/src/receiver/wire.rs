//! Wire protocol types for the receiver role.
//!
//! Defines the signature header (`SumHead`), sender response attributes
//! (`SenderAttrs`), and the signature block writer used during the
//! request/response exchange between receiver and sender.

use std::io::{self, Read, Write};

use engine::signature::FileSignature;
use protocol::codec::NdxCodec;
use protocol::read_varint;
use protocol::xattr::XattrList;

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

    /// Reads a sum_head from the wire in rsync format.
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;
        Ok(Self {
            count: i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as u32,
            blength: i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u32,
            s2length: i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as u32,
            remainder: i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as u32,
        })
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
            Some(protocol::FnameCmpType::from_wire(byte[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid fnamecmp type: 0x{:02X} {}{}",
                        byte[0],
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?)
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
                ((len_byte[0] & 0x7F) as usize) * 256 + second_byte[0] as usize
            } else {
                len_byte[0] as usize
            };
            // Upstream MAXPATHLEN is typically 4096; reject excessively long names
            const MAX_XNAME_LEN: usize = 4096;
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
        let xattr_values = if preserve_xattrs
            && (iflags & Self::ITEM_REPORT_XATTR != 0)
            && !(want_xattr_optim
                && (iflags & Self::ITEM_XNAME_FOLLOWS != 0)
                && (iflags & Self::ITEM_LOCAL_CHANGE != 0))
        {
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
        // upstream: num += rel_pos (delta-encoded 1-based entry numbers)
        let num = prior_req + rel_pos;
        prior_req = num;

        // upstream: rxa->datum_len = read_varint(f_in); read_buf(f_in, rxa->datum, rxa->datum_len)
        let datum_len = read_varint(reader)? as usize;
        let mut value = vec![0u8; datum_len];
        reader.read_exact(&mut value)?;

        values.push((num, value));
    }

    Ok(values)
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
