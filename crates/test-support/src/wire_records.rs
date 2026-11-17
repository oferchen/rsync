//! Reads the generator's itemize records out of a captured rsync stream.
//!
//! A test that tees what one side sends can compare it with what rsync sends
//! in the same place, record by record, where a log or a printed row would
//! hide a flag bit or the order.
//!
//! The stream is the multiplexed one a receiver writes: [`demultiplex`] strips
//! the framing, [`skip_filter_list`] steps over the filter list a client
//! sends first, and [`generator_records`] decodes each index, its item flags
//! and the name that may follow, skipping the checksum data of a transfer
//! request.

/// One record the generator sent: the entry's index, its item flags and the
/// name that follows them, if any.
pub type Record = (i32, u16, Option<Vec<u8>>);

/// The item flag of a record that requests a transfer (`rsync.h`).
pub const ITEM_TRANSFER: u16 = 1 << 15;
const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;
const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;

/// Strips the multiplex framing from a captured stream and returns its data
/// channel.
///
/// The handshake ahead of the first frame is skipped by finding the offset
/// from which the rest parses as whole frames.
///
/// # Panics
///
/// Panics when no offset in the first 200 bytes starts such a sequence.
#[must_use]
pub fn demultiplex(raw: &[u8]) -> Vec<u8> {
    const MPLEX_BASE: u32 = 7;
    'start: for start in 0..raw.len().min(200) {
        let mut data = Vec::new();
        let mut pos = start;
        while pos < raw.len() {
            let Some(header) = raw.get(pos..pos + 4) else {
                continue 'start;
            };
            let header = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            let len = (header & 0x00ff_ffff) as usize;
            let Some(payload) = raw.get(pos + 4..pos + 4 + len) else {
                continue 'start;
            };
            match (header >> 24).checked_sub(MPLEX_BASE) {
                Some(0) => data.extend_from_slice(payload),
                Some(tag) if tag <= 120 => {}
                _ => continue 'start,
            }
            pos += 4 + len;
        }
        return data;
    }
    panic!("no multiplexed stream in {} captured bytes", raw.len());
}

/// A read position in the data channel.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> &'a [u8] {
        let (bytes, rest) = self.0.split_at(count);
        self.0 = rest;
        bytes
    }

    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }

    fn word(&mut self) -> u32 {
        let bytes = self.take(4);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

/// Returns `data` past the filter list a client sends ahead of its records:
/// length-prefixed rules up to a zero length (`exclude.c:send_filter_list()`).
///
/// # Panics
///
/// Panics when `data` ends inside the list.
#[must_use]
pub fn skip_filter_list(data: &[u8]) -> &[u8] {
    let mut cursor = Cursor(data);
    loop {
        let rule_len = cursor.word();
        if rule_len == 0 {
            return cursor.0;
        }
        cursor.take(rule_len as usize);
    }
}

/// Decodes the generator's records from `data`.
///
/// Each record is an index (`io.c:read_ndx()`), the item flags, the optional
/// basis type and name (`generator.c:625-640`) and, for a transfer request
/// that is not a dry run, the checksum head and its sums
/// (`io.c:write_sum_head()`), which are skipped. `NDX_DONE` and the negative
/// indexes carry no record and are passed over.
///
/// # Panics
///
/// Panics when `data` ends inside a record.
#[must_use]
pub fn generator_records(data: &[u8]) -> Vec<Record> {
    let mut cursor = Cursor(data);
    let mut found = Vec::new();
    let (mut prev_positive, mut prev_negative) = (-1i32, 1i32);
    while !cursor.0.is_empty() {
        let first = cursor.byte();
        if first == 0 {
            continue;
        }
        let negative = first == 0xff;
        let lead = if negative { cursor.byte() } else { first };
        let prev = if negative {
            prev_negative
        } else {
            prev_positive
        };
        let number = if lead == 0xfe {
            let head = cursor.take(2);
            if head[0] & 0x80 != 0 {
                let rest = cursor.take(2);
                i32::from_le_bytes([head[1], rest[0], rest[1], head[0] & 0x7f])
            } else {
                (i32::from(head[0]) << 8) + i32::from(head[1]) + prev
            }
        } else {
            i32::from(lead) + prev
        };
        if negative {
            prev_negative = number;
            continue;
        }
        prev_positive = number;
        let flags = cursor.take(2);
        let iflags = u16::from_le_bytes([flags[0], flags[1]]);
        if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
            cursor.byte();
        }
        let xname = (iflags & ITEM_XNAME_FOLLOWS != 0).then(|| {
            let mut len = usize::from(cursor.byte());
            if len & 0x80 != 0 {
                len = ((len & 0x7f) << 8) | usize::from(cursor.byte());
            }
            cursor.take(len).to_vec()
        });
        if iflags & ITEM_TRANSFER != 0 {
            let count = cursor.word() as usize;
            let _block_len = cursor.word();
            let strong_len = cursor.word() as usize;
            let _remainder = cursor.word();
            cursor.take(count * (4 + strong_len));
        }
        found.push((number, iflags, xname));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame is a little-endian length with the channel in the top byte.
    fn frame(channel: u8, payload: &[u8]) -> Vec<u8> {
        let header = (u32::from(channel + 7) << 24) | payload.len() as u32;
        [&header.to_le_bytes()[..], payload].concat()
    }

    #[test]
    fn demultiplex_joins_data_frames_and_drops_the_handshake_and_messages() {
        let mut raw = b"\x21\x00\x00\x00handshake".to_vec();
        raw.extend(frame(0, b"ab"));
        raw.extend(frame(2, b"a message"));
        raw.extend(frame(0, b"cd"));
        assert_eq!(demultiplex(&raw), b"abcd");
    }

    #[test]
    fn the_filter_list_ends_at_a_zero_length() {
        let data = [&[3, 0, 0, 0][..], b"- x", &[0, 0, 0, 0], b"rest"].concat();
        assert_eq!(skip_filter_list(&data), b"rest");
    }

    /// Bytes rsync 3.5.2 sent on a `-aiH` pull: a linked follower with its
    /// leader's name, a transfer with one block sum, and a follower record
    /// on an absolute index, with an `NDX_DONE` between them.
    #[test]
    fn records_carry_index_flags_and_name_and_skip_the_sums() {
        let data = [
            &[0x04, 0x00, 0x70, 0x02, b'a', b'1'][..],
            &[0x03, 0x0c, 0x80],
            &[1, 0, 0, 0, 0xbc, 0x02, 0, 0, 2, 0, 0, 0, 12, 0, 0, 0],
            &[0xed, 0x03, 0xd2, 0x19, 0xb3, 0xb8],
            &[0x00],
            &[0xfe, 0x80, 0x09, 0x00, 0x00, 0x0c, 0x50, 0x02, b'c', b'1'],
        ]
        .concat();
        assert_eq!(
            generator_records(&data),
            [
                (3, 0x7000, Some(b"a1".to_vec())),
                (6, 0x800c, None),
                (9, 0x500c, Some(b"c1".to_vec())),
            ]
        );
    }
}
