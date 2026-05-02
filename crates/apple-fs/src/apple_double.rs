//! AppleDouble v2 (RFC 1740) container parser and encoder.
//!
//! AppleDouble is the on-disk format used by macOS to store the resource
//! fork, Finder info, and other Mac-specific metadata when writing to a
//! filesystem that does not natively support extended attributes (SMB, FAT,
//! some NFS exports, older Linux NFS clients). Each metadata-bearing file
//! `foo` is paired with a sidecar named `._foo` containing an AppleDouble v2
//! header followed by one or more entry payloads.
//!
//! This module implements only the container format. It does not decide
//! whether or how to merge sidecar payloads back into a destination file's
//! native extended attributes - that is an interoperability policy decision
//! that lives in the transfer pipeline (see `docs/audits/apple-fs-roundtrip.md`,
//! finding F-2). The pure-data parser and encoder here are intended for
//! inspection tools, audit harnesses, and any future merge implementation.
//!
//! # Wire layout (RFC 1740 section 5)
//!
//! ```text
//! offset  size  field
//! 0       4     magic number (big-endian, 0x00051607 for AppleDouble)
//! 4       4     version number (big-endian, 0x00020000 for v2)
//! 8       16    filler (zero)
//! 24      2     number of entries (big-endian)
//! 26      N*12  entry descriptors:
//!                 +0  4  entry id (big-endian)
//!                 +4  4  payload offset from start of file (big-endian)
//!                 +8  4  payload length (big-endian)
//! ```
//!
//! Entry payloads follow the descriptor table. Their order is unspecified by
//! the RFC; this encoder writes them in entry-id order for determinism.
//!
//! # References
//!
//! - RFC 1740: "MIME Encapsulation of Macintosh Files - MacMIME", section 5.
//! - Apple Technical Note TN1188: "AppleSingle/AppleDouble Formats".

use std::io;

/// Magic number identifying an AppleDouble v2 container.
pub const APPLE_DOUBLE_MAGIC: u32 = 0x0005_1607;

/// Magic number identifying an AppleSingle v2 container.
///
/// Provided for completeness: `oc-rsync` only emits AppleDouble, but parsers
/// in the wild may receive either form.
pub const APPLE_SINGLE_MAGIC: u32 = 0x0005_1600;

/// Version number for AppleDouble / AppleSingle v2.
pub const APPLE_DOUBLE_VERSION_2: u32 = 0x0002_0000;

/// Size of the fixed-length header (magic + version + filler + entry count).
pub const HEADER_SIZE: usize = 26;

/// Size of one entry descriptor (id + offset + length).
pub const ENTRY_DESCRIPTOR_SIZE: usize = 12;

/// Standard AppleDouble entry identifiers (RFC 1740 section 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
#[allow(missing_docs)]
pub enum EntryId {
    DataFork = 1,
    ResourceFork = 2,
    RealName = 3,
    Comment = 4,
    IconBw = 5,
    IconColor = 6,
    FileDatesInfo = 8,
    FinderInfo = 9,
    MacFileInfo = 10,
    ProDosFileInfo = 11,
    MsDosFileInfo = 12,
    ShortName = 13,
    AfpFileInfo = 14,
    DirectoryId = 15,
}

impl EntryId {
    /// Returns the well-known [`EntryId`] for the given numeric identifier,
    /// or `None` for vendor-specific or reserved values.
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::DataFork,
            2 => Self::ResourceFork,
            3 => Self::RealName,
            4 => Self::Comment,
            5 => Self::IconBw,
            6 => Self::IconColor,
            8 => Self::FileDatesInfo,
            9 => Self::FinderInfo,
            10 => Self::MacFileInfo,
            11 => Self::ProDosFileInfo,
            12 => Self::MsDosFileInfo,
            13 => Self::ShortName,
            14 => Self::AfpFileInfo,
            15 => Self::DirectoryId,
            _ => return None,
        })
    }
}

/// One AppleDouble entry: a typed payload with a stable identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Numeric identifier. Use [`EntryId`] for the standard values.
    pub id: u32,
    /// Raw payload bytes for this entry.
    pub data: Vec<u8>,
}

impl Entry {
    /// Constructs an entry from a well-known [`EntryId`] and payload.
    pub fn new(id: EntryId, data: Vec<u8>) -> Self {
        Self {
            id: id as u32,
            data,
        }
    }

    /// Constructs an entry from a raw numeric identifier and payload.
    ///
    /// Use this for vendor-specific or reserved IDs that are not part of
    /// [`EntryId`].
    pub fn from_raw(id: u32, data: Vec<u8>) -> Self {
        Self { id, data }
    }

    /// Returns the well-known [`EntryId`] for this entry, when applicable.
    pub fn standard_id(&self) -> Option<EntryId> {
        EntryId::from_u32(self.id)
    }
}

/// A decoded AppleDouble v2 container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppleDouble {
    /// Entries in encounter order. The container preserves insertion order
    /// when round-tripping, but sorts by `id` when [`encode`](Self::encode) is
    /// called for byte-stable output.
    pub entries: Vec<Entry>,
}

impl AppleDouble {
    /// Constructs an empty container.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the first entry with the given identifier, if present.
    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        let raw = id as u32;
        self.entries.iter().find(|entry| entry.id == raw)
    }

    /// Returns the resource-fork payload, if present.
    pub fn resource_fork(&self) -> Option<&[u8]> {
        self.entry(EntryId::ResourceFork).map(|e| e.data.as_slice())
    }

    /// Returns the Finder-info payload, if present.
    ///
    /// Finder info is canonically 32 bytes. The accessor returns the raw
    /// payload regardless of length so callers can validate as appropriate.
    pub fn finder_info(&self) -> Option<&[u8]> {
        self.entry(EntryId::FinderInfo).map(|e| e.data.as_slice())
    }

    /// Adds or replaces the entry with the given identifier.
    pub fn set_entry(&mut self, id: EntryId, data: Vec<u8>) {
        let raw = id as u32;
        if let Some(slot) = self.entries.iter_mut().find(|entry| entry.id == raw) {
            slot.data = data;
        } else {
            self.entries.push(Entry::new(id, data));
        }
    }

    /// Decodes an AppleDouble or AppleSingle v2 byte stream.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] of kind [`io::ErrorKind::InvalidData`] when:
    /// - the input is shorter than the fixed header,
    /// - the magic number does not match either container variant,
    /// - the version is not 0x00020000,
    /// - the entry-descriptor table is truncated, or
    /// - an entry's declared offset/length exceeds the input length.
    pub fn decode(input: &[u8]) -> io::Result<Self> {
        if input.len() < HEADER_SIZE {
            return Err(invalid("AppleDouble header truncated"));
        }
        let magic = read_u32(&input[0..4]);
        if magic != APPLE_DOUBLE_MAGIC && magic != APPLE_SINGLE_MAGIC {
            return Err(invalid("AppleDouble magic number mismatch"));
        }
        let version = read_u32(&input[4..8]);
        if version != APPLE_DOUBLE_VERSION_2 {
            return Err(invalid("AppleDouble version is not 2"));
        }
        // Bytes 8..24 are the filler; the spec says callers must ignore them.
        let entry_count = u16::from_be_bytes([input[24], input[25]]) as usize;
        let descriptors_end = HEADER_SIZE
            .checked_add(
                entry_count
                    .checked_mul(ENTRY_DESCRIPTOR_SIZE)
                    .ok_or_else(|| invalid("AppleDouble descriptor table size overflows"))?,
            )
            .ok_or_else(|| invalid("AppleDouble descriptor table size overflows"))?;
        if input.len() < descriptors_end {
            return Err(invalid("AppleDouble descriptor table truncated"));
        }

        let mut entries = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let base = HEADER_SIZE + index * ENTRY_DESCRIPTOR_SIZE;
            let id = read_u32(&input[base..base + 4]);
            let offset = read_u32(&input[base + 4..base + 8]) as usize;
            let length = read_u32(&input[base + 8..base + 12]) as usize;
            let end = offset
                .checked_add(length)
                .ok_or_else(|| invalid("AppleDouble entry offset+length overflows"))?;
            if end > input.len() {
                return Err(invalid("AppleDouble entry payload extends past input"));
            }
            entries.push(Entry::from_raw(id, input[offset..end].to_vec()));
        }
        Ok(Self { entries })
    }

    /// Encodes the container into an AppleDouble v2 byte stream.
    ///
    /// Entries are written in ascending identifier order so that two
    /// containers with the same logical content always produce identical
    /// bytes. Payloads are laid out contiguously after the descriptor table.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] of kind [`io::ErrorKind::InvalidData`] when
    /// the encoded layout would exceed `u32::MAX` bytes (the wire-format
    /// width of `offset` and `length`) or contain more than `u16::MAX` entries.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if self.entries.len() > u16::MAX as usize {
            return Err(invalid("AppleDouble container has too many entries"));
        }

        // Sort a copy by id for byte-stable output without mutating the caller.
        let mut ordered: Vec<&Entry> = self.entries.iter().collect();
        ordered.sort_by_key(|entry| entry.id);

        let entry_count = ordered.len();
        let descriptors_end = HEADER_SIZE + entry_count * ENTRY_DESCRIPTOR_SIZE;
        let total: usize = ordered.iter().map(|entry| entry.data.len()).sum();
        let total_len = descriptors_end
            .checked_add(total)
            .ok_or_else(|| invalid("AppleDouble container size overflows"))?;
        if total_len > u32::MAX as usize {
            return Err(invalid("AppleDouble container size exceeds u32 range"));
        }

        let mut out = Vec::with_capacity(total_len);
        out.extend_from_slice(&APPLE_DOUBLE_MAGIC.to_be_bytes());
        out.extend_from_slice(&APPLE_DOUBLE_VERSION_2.to_be_bytes());
        out.extend_from_slice(&[0u8; 16]); // filler
        out.extend_from_slice(&(entry_count as u16).to_be_bytes());

        let mut payload_offset = descriptors_end as u32;
        for entry in &ordered {
            let length = u32::try_from(entry.data.len())
                .map_err(|_| invalid("AppleDouble entry payload exceeds u32 range"))?;
            out.extend_from_slice(&entry.id.to_be_bytes());
            out.extend_from_slice(&payload_offset.to_be_bytes());
            out.extend_from_slice(&length.to_be_bytes());
            payload_offset = payload_offset
                .checked_add(length)
                .ok_or_else(|| invalid("AppleDouble payload offset overflows"))?;
        }
        for entry in &ordered {
            out.extend_from_slice(&entry.data);
        }
        Ok(out)
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_finder_info() -> Vec<u8> {
        // 32-byte canonical FinderInfo: file type "TEXT", creator "ttxt".
        let mut info = vec![0u8; 32];
        info[0..4].copy_from_slice(b"TEXT");
        info[4..8].copy_from_slice(b"ttxt");
        info
    }

    fn sample_resource_fork() -> Vec<u8> {
        // A trivial empty resource map - 256 bytes is enough for shape tests.
        (0..256u16).map(|i| (i & 0xff) as u8).collect()
    }

    #[test]
    fn round_trip_finder_info_only() {
        let mut container = AppleDouble::new();
        container.set_entry(EntryId::FinderInfo, sample_finder_info());

        let encoded = container.encode().expect("encode");
        let decoded = AppleDouble::decode(&encoded).expect("decode");
        assert_eq!(decoded, container);
        assert_eq!(decoded.finder_info().unwrap().len(), 32);
    }

    #[test]
    fn round_trip_resource_fork_and_finder_info() {
        let mut container = AppleDouble::new();
        container.set_entry(EntryId::ResourceFork, sample_resource_fork());
        container.set_entry(EntryId::FinderInfo, sample_finder_info());

        let encoded = container.encode().expect("encode");
        let decoded = AppleDouble::decode(&encoded).expect("decode");
        assert_eq!(
            decoded.resource_fork().unwrap(),
            &sample_resource_fork()[..]
        );
        assert_eq!(decoded.finder_info().unwrap(), &sample_finder_info()[..]);
    }

    #[test]
    fn header_layout_is_stable() {
        let mut container = AppleDouble::new();
        container.set_entry(EntryId::FinderInfo, sample_finder_info());
        let encoded = container.encode().expect("encode");
        assert_eq!(read_u32(&encoded[0..4]), APPLE_DOUBLE_MAGIC);
        assert_eq!(read_u32(&encoded[4..8]), APPLE_DOUBLE_VERSION_2);
        assert_eq!(&encoded[8..24], &[0u8; 16]);
        assert_eq!(u16::from_be_bytes([encoded[24], encoded[25]]), 1);
    }

    #[test]
    fn encode_orders_entries_by_id() {
        let mut container = AppleDouble::new();
        container.set_entry(EntryId::FinderInfo, sample_finder_info()); // id 9
        container.set_entry(EntryId::ResourceFork, sample_resource_fork()); // id 2

        let encoded = container.encode().expect("encode");
        // First descriptor's id field starts at HEADER_SIZE.
        let first_id = read_u32(&encoded[HEADER_SIZE..HEADER_SIZE + 4]);
        assert_eq!(first_id, EntryId::ResourceFork as u32);
        let second_id = read_u32(
            &encoded[HEADER_SIZE + ENTRY_DESCRIPTOR_SIZE..HEADER_SIZE + ENTRY_DESCRIPTOR_SIZE + 4],
        );
        assert_eq!(second_id, EntryId::FinderInfo as u32);
    }

    #[test]
    fn set_entry_replaces_existing() {
        let mut container = AppleDouble::new();
        container.set_entry(EntryId::FinderInfo, vec![1; 32]);
        container.set_entry(EntryId::FinderInfo, vec![2; 32]);
        assert_eq!(container.entries.len(), 1);
        assert_eq!(container.finder_info().unwrap(), &[2; 32][..]);
    }

    #[test]
    fn decode_rejects_short_input() {
        let err = AppleDouble::decode(&[0u8; 8]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = vec![0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&0xdead_beef_u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&APPLE_DOUBLE_VERSION_2.to_be_bytes());
        let err = AppleDouble::decode(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_bad_version() {
        let mut bytes = vec![0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&APPLE_DOUBLE_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
        let err = AppleDouble::decode(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_truncated_descriptor_table() {
        let mut bytes = vec![0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&APPLE_DOUBLE_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&APPLE_DOUBLE_VERSION_2.to_be_bytes());
        // Claim 2 descriptors but provide none of their bytes.
        bytes[24..26].copy_from_slice(&2u16.to_be_bytes());
        let err = AppleDouble::decode(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_payload_past_end() {
        let mut bytes = vec![0u8; HEADER_SIZE + ENTRY_DESCRIPTOR_SIZE];
        bytes[0..4].copy_from_slice(&APPLE_DOUBLE_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&APPLE_DOUBLE_VERSION_2.to_be_bytes());
        bytes[24..26].copy_from_slice(&1u16.to_be_bytes());
        // Descriptor: id=9, offset=HEADER_SIZE+ENTRY_DESCRIPTOR_SIZE, length=999.
        bytes[26..30].copy_from_slice(&9u32.to_be_bytes());
        bytes[30..34]
            .copy_from_slice(&((HEADER_SIZE + ENTRY_DESCRIPTOR_SIZE) as u32).to_be_bytes());
        bytes[34..38].copy_from_slice(&999u32.to_be_bytes());
        let err = AppleDouble::decode(&bytes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn entry_id_round_trip_for_known_values() {
        for id in [
            EntryId::DataFork,
            EntryId::ResourceFork,
            EntryId::RealName,
            EntryId::Comment,
            EntryId::IconBw,
            EntryId::IconColor,
            EntryId::FileDatesInfo,
            EntryId::FinderInfo,
            EntryId::MacFileInfo,
            EntryId::ProDosFileInfo,
            EntryId::MsDosFileInfo,
            EntryId::ShortName,
            EntryId::AfpFileInfo,
            EntryId::DirectoryId,
        ] {
            let raw = id as u32;
            assert_eq!(EntryId::from_u32(raw), Some(id));
        }
    }

    #[test]
    fn entry_id_unknown_returns_none() {
        assert_eq!(EntryId::from_u32(0), None);
        assert_eq!(EntryId::from_u32(7), None);
        assert_eq!(EntryId::from_u32(99), None);
    }

    #[test]
    fn apple_single_magic_is_accepted_by_decoder() {
        let mut bytes = vec![0u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&APPLE_SINGLE_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&APPLE_DOUBLE_VERSION_2.to_be_bytes());
        // zero entries
        let decoded = AppleDouble::decode(&bytes).expect("decode applesingle");
        assert!(decoded.entries.is_empty());
    }
}
