//! UID/GID mapping lists for rsync protocol.
//!
//! This module implements the ID list structures used in rsync's UID/GID name
//! mapping feature. When `--numeric-ids` is not set, rsync transmits user and
//! group names so the receiver can map them to local IDs.
//!
//! # Wire Format
//!
//! The wire format is protocol version dependent (`varint30` pattern):
//!
//! **Protocol < 30 (legacy):**
//! - ID lists use fixed 4-byte little-endian integers
//! - `(int32 id, byte name_len, name_bytes)` tuples, terminated by `int32 0`
//!
//! **Protocol >= 30 (modern):**
//! - ID lists use variable-length integers
//! - `(varint id, byte name_len, name_bytes)` tuples, terminated by `varint 0`
//!
//! With the `ID0_NAMES` compat flag (protocol 30+), an additional name for id=0
//! follows the terminator.
//!
//! # Upstream Reference
//!
//! - `uidlist.c` - UID/GID list management
//! - `io.h:21-43` - `read_varint30()`/`write_varint30()` inline functions

pub mod trace;

pub use trace::{IdKind, trace_id_maps_to, trace_process_gids, trace_set_gid, trace_set_uid};

use std::collections::HashMap;
use std::io::{self, Read, Write};

/// Defence-in-depth cap on the number of UID/GID mapping entries from the wire.
///
/// A typical system has fewer than a thousand unique users/groups. 65536 is well
/// above any real-world usage while preventing a malicious peer from forcing
/// unbounded allocations by sending millions of id-name pairs without ever
/// sending the terminating zero.
///
/// upstream: uidlist.c `recv_id_list()` loops until id==0 with no explicit
/// count cap; relies on the sender eventually terminating.
const MAX_WIRE_ID_LIST_ENTRIES: usize = 65536;

/// A mapping from a remote ID to its name and resolved local ID.
#[derive(Debug, Clone)]
struct IdEntry {
    /// The name associated with this ID (from remote system).
    name: Option<Vec<u8>>,
    /// The local ID that this remote ID maps to.
    local_id: u32,
}

/// Collects and maps UID or GID values between systems.
///
/// This structure serves two purposes:
/// 1. On the sender side: collects IDs encountered in the file list and looks up their names
/// 2. On the receiver side: stores received mappings and resolves remote IDs to local IDs
///
/// # Example (Sender)
///
/// ```ignore
/// let mut id_list = IdList::new();
/// // During file list building, collect UIDs
/// id_list.add_id(1000, lookup_user_name(1000).ok().flatten());
/// id_list.add_id(1001, lookup_user_name(1001).ok().flatten());
/// // Send the list
/// id_list.write(&mut writer, false)?;
/// ```
///
/// # Example (Receiver)
///
/// ```ignore
/// let mut id_list = IdList::new();
/// // Read the list from sender
/// id_list.read(&mut reader, false, |name| lookup_user_by_name(name).ok().flatten())?;
/// // Later, when applying ownership:
/// let local_uid = id_list.match_id(remote_uid);
/// ```
#[derive(Debug, Default)]
pub struct IdList {
    /// Maps remote ID to entry (name and local ID).
    entries: HashMap<u32, IdEntry>,
    /// Ordered list of IDs for deterministic sending.
    order: Vec<u32>,
}

impl IdList {
    /// Creates a new empty ID list.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of IDs in the list.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the list is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns true if the list already contains this ID.
    ///
    /// Use this to check before performing expensive name lookups.
    #[inline]
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        self.entries.contains_key(&id)
    }

    /// Adds an ID with its associated name to the list.
    ///
    /// If the ID already exists, this is a no-op (preserving first occurrence).
    /// This matches upstream rsync's behavior where each unique ID is only sent once.
    ///
    /// # Arguments
    ///
    /// * `id` - The numeric ID (UID or GID)
    /// * `name` - The name associated with this ID, or None if lookup failed
    pub fn add_id(&mut self, id: u32, name: Option<Vec<u8>>) {
        if self.entries.contains_key(&id) {
            return;
        }
        self.entries.insert(
            id,
            IdEntry {
                name,
                local_id: id, // Default to same ID until matched
            },
        );
        self.order.push(id);
    }

    /// Looks up the local ID for a given remote ID.
    ///
    /// Returns the mapped local ID if the remote ID was in the received list,
    /// otherwise returns the remote ID unchanged (fallback to numeric ID).
    #[inline]
    #[must_use]
    pub fn match_id(&self, remote_id: u32) -> u32 {
        self.entries
            .get(&remote_id)
            .map(|e| e.local_id)
            .unwrap_or(remote_id)
    }

    /// Writes the ID list to the wire.
    ///
    /// # Wire Format
    ///
    /// For protocol < 30 (varint30 fallback):
    /// - `int32 id` - The numeric ID as 4-byte little-endian
    /// - `byte len` - Name length (0-255)
    /// - `bytes[len]` - The name
    ///
    /// For protocol >= 30:
    /// - `varint id` - The numeric ID
    /// - `byte len` - Name length (0-255)
    /// - `bytes[len]` - The name
    ///
    /// Terminated by id=0 (encoded per protocol version).
    ///
    /// If `id0_names` is true, also sends the name for id=0 after the terminator.
    ///
    /// # Arguments
    ///
    /// * `writer` - The destination for encoded data
    /// * `id0_names` - Whether to send id=0's name (ID0_NAMES compat flag)
    /// * `protocol_version` - The negotiated protocol version (affects encoding)
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:382` - `send_user_name()` uses `write_varint30()`
    /// - `io.h:37` - `write_varint30()` switches between int and varint at protocol 30
    pub fn write<W: Write>(
        &self,
        writer: &mut W,
        id0_names: bool,
        protocol_version: u8,
    ) -> io::Result<()> {
        // Send (id, name) pairs for non-zero IDs with names
        for &id in &self.order {
            if id == 0 {
                continue; // id=0 is handled separately
            }
            if let Some(entry) = self.entries.get(&id) {
                if let Some(ref name) = entry.name {
                    let len = name.len().min(255) as u8;
                    // Use varint30 encoding (int for proto < 30, varint for proto >= 30)
                    crate::write_varint30_int(writer, id as i32, protocol_version)?;
                    writer.write_all(&[len])?;
                    if len > 0 {
                        writer.write_all(&name[..len as usize])?;
                    }
                }
            }
        }

        // Terminate with id=0
        crate::write_varint30_int(writer, 0, protocol_version)?;

        // With ID0_NAMES, send id=0's name
        if id0_names {
            if let Some(entry) = self.entries.get(&0) {
                if let Some(ref name) = entry.name {
                    let len = name.len().min(255) as u8;
                    writer.write_all(&[len])?;
                    if len > 0 {
                        writer.write_all(&name[..len as usize])?;
                    }
                } else {
                    writer.write_all(&[0])?;
                }
            } else {
                writer.write_all(&[0])?;
            }
        }

        Ok(())
    }

    /// Reads an ID list from the wire and resolves names to local IDs.
    ///
    /// # Arguments
    ///
    /// * `reader` - The source of encoded data
    /// * `id0_names` - Whether to read id=0's name (ID0_NAMES compat flag)
    /// * `protocol_version` - The negotiated protocol version (affects decoding)
    /// * `name_to_id` - Function to resolve a name to a local ID
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:467` - `recv_id_list()` uses `read_varint30()`
    /// - `io.h:21` - `read_varint30()` switches between int and varint at protocol 30
    pub fn read<R: Read + ?Sized, F>(
        &mut self,
        reader: &mut R,
        id0_names: bool,
        protocol_version: u8,
        name_to_id: F,
    ) -> io::Result<()>
    where
        F: Fn(&[u8]) -> Option<u32>,
    {
        self.read_with_kind(reader, id0_names, protocol_version, None, name_to_id)
    }

    /// Reads an ID list from the wire, emitting `--debug=OWN` traces.
    ///
    /// Behaves identically to [`IdList::read`] but additionally fires the
    /// upstream `DEBUG_GTE(OWN, 2)` per-entry trace for every id resolved.
    /// Callers that want the mapping diagnostics pass `Some(IdKind::Uid)` or
    /// `Some(IdKind::Gid)`; passing `None` is equivalent to [`IdList::read`].
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:243-294` - `recv_add_id()` performs the resolution and
    ///   emits `"%sid %u(%s) maps to %u"` at level 2 (line 287).
    pub fn read_with_kind<R: Read + ?Sized, F>(
        &mut self,
        reader: &mut R,
        id0_names: bool,
        protocol_version: u8,
        kind: Option<IdKind>,
        name_to_id: F,
    ) -> io::Result<()>
    where
        F: Fn(&[u8]) -> Option<u32>,
    {
        // Read (id, name) pairs until id=0
        let mut count = 0usize;
        loop {
            // Use varint30 decoding (int for proto < 30, varint for proto >= 30)
            let id_signed = crate::read_varint30_int(reader, protocol_version)?;
            if id_signed == 0 {
                break;
            }

            // Defence-in-depth: reject unreasonably large ID lists to prevent
            // a malicious peer from forcing unbounded allocations.
            count += 1;
            if count > MAX_WIRE_ID_LIST_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("ID list entry count exceeds maximum {MAX_WIRE_ID_LIST_ENTRIES}"),
                ));
            }

            // IDs are non-negative, convert from wire format
            let id = id_signed as u32;

            let (name, local_id) = self.read_name_and_resolve(reader, id, &name_to_id)?;
            // upstream: uidlist.c:287-291 - `"%sid %u(%s) maps to %u"`.
            if let Some(k) = kind {
                trace_id_maps_to(k, id, name.as_deref(), local_id);
            }
            self.entries.insert(id, IdEntry { name, local_id });
            self.order.push(id);
        }

        // With ID0_NAMES, read id=0's name
        if id0_names {
            let (name, local_id) = self.read_name_and_resolve(reader, 0, &name_to_id)?;
            // upstream: uidlist.c:287-291 - the id=0 entry is processed by the
            // same `recv_add_id` path and therefore also fires the level-2 trace.
            if let Some(k) = kind {
                trace_id_maps_to(k, 0, name.as_deref(), local_id);
            }
            self.entries.insert(0, IdEntry { name, local_id });
            self.order.push(0);
        }

        Ok(())
    }

    /// Reads a name from the wire and resolves it to a local ID.
    fn read_name_and_resolve<R: Read + ?Sized, F>(
        &self,
        reader: &mut R,
        remote_id: u32,
        name_to_id: &F,
    ) -> io::Result<(Option<Vec<u8>>, u32)>
    where
        F: Fn(&[u8]) -> Option<u32>,
    {
        let mut len_buf = [0u8; 1];
        reader.read_exact(&mut len_buf)?;
        let len = len_buf[0] as usize;

        if len == 0 {
            return Ok((None, remote_id));
        }

        let mut name = vec![0u8; len];
        reader.read_exact(&mut name)?;

        // Try to resolve the name to a local ID
        let local_id = name_to_id(&name).unwrap_or(remote_id);

        Ok((Some(name), local_id))
    }

    /// Clears all entries from the list.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_list_is_empty() {
        let list = IdList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn add_id_increases_length() {
        let mut list = IdList::new();
        list.add_id(1000, Some(b"testuser".to_vec()));
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
    }

    #[test]
    fn add_duplicate_id_is_noop() {
        let mut list = IdList::new();
        list.add_id(1000, Some(b"testuser".to_vec()));
        list.add_id(1000, Some(b"different".to_vec()));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn match_id_returns_same_id_by_default() {
        let list = IdList::new();
        assert_eq!(list.match_id(1000), 1000);
    }

    #[test]
    fn match_id_returns_local_id_after_add() {
        let mut list = IdList::new();
        list.add_id(1000, Some(b"testuser".to_vec()));
        // Before read/resolve, local_id defaults to remote_id
        assert_eq!(list.match_id(1000), 1000);
    }

    #[test]
    fn write_empty_list_proto30() {
        let list = IdList::new();
        let mut buf = Vec::new();
        list.write(&mut buf, false, 30).unwrap();
        // Should just be the terminator (varint 0)
        assert_eq!(buf, vec![0]);
    }

    #[test]
    fn write_empty_list_proto29() {
        let list = IdList::new();
        let mut buf = Vec::new();
        list.write(&mut buf, false, 29).unwrap();
        // Should be 4-byte int terminator for protocol < 30
        assert_eq!(buf, vec![0, 0, 0, 0]);
    }

    #[test]
    fn write_single_id_proto30() {
        let mut list = IdList::new();
        list.add_id(1, Some(b"root".to_vec()));
        let mut buf = Vec::new();
        list.write(&mut buf, false, 30).unwrap();
        // varint(1), len(4), "root", varint(0)
        assert_eq!(buf, vec![1, 4, b'r', b'o', b'o', b't', 0]);
    }

    #[test]
    fn write_single_id_proto29() {
        let mut list = IdList::new();
        list.add_id(1, Some(b"root".to_vec()));
        let mut buf = Vec::new();
        list.write(&mut buf, false, 29).unwrap();
        // int32(1), len(4), "root", int32(0)
        assert_eq!(buf, vec![1, 0, 0, 0, 4, b'r', b'o', b'o', b't', 0, 0, 0, 0]);
    }

    #[test]
    fn write_with_id0_names() {
        let mut list = IdList::new();
        list.add_id(0, Some(b"root".to_vec()));
        let mut buf = Vec::new();
        list.write(&mut buf, true, 30).unwrap();
        // varint(0) terminator, len(4), "root"
        assert_eq!(buf, vec![0, 4, b'r', b'o', b'o', b't']);
    }

    #[test]
    fn read_empty_list_proto30() {
        let data = vec![0u8]; // Just terminator (varint)
        let mut list = IdList::new();
        list.read(&mut data.as_slice(), false, 30, |_| None)
            .unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn read_empty_list_proto29() {
        let data = vec![0u8, 0, 0, 0]; // Just terminator (4-byte int)
        let mut list = IdList::new();
        list.read(&mut data.as_slice(), false, 29, |_| None)
            .unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn read_single_id_proto30() {
        // varint(1), len(4), "root", varint(0)
        let data = vec![1, 4, b'r', b'o', b'o', b't', 0];
        let mut list = IdList::new();
        list.read(&mut data.as_slice(), false, 30, |_| Some(0))
            .unwrap();
        assert_eq!(list.len(), 1);
        // Name resolved to 0
        assert_eq!(list.match_id(1), 0);
    }

    #[test]
    fn read_single_id_proto29() {
        // int32(1), len(4), "root", int32(0)
        let data = vec![1, 0, 0, 0, 4, b'r', b'o', b'o', b't', 0, 0, 0, 0];
        let mut list = IdList::new();
        list.read(&mut data.as_slice(), false, 29, |_| Some(0))
            .unwrap();
        assert_eq!(list.len(), 1);
        // Name resolved to 0
        assert_eq!(list.match_id(1), 0);
    }

    #[test]
    fn read_with_id0_names() {
        // varint(0) terminator, len(4), "root"
        let data = vec![0, 4, b'r', b'o', b'o', b't'];
        let mut list = IdList::new();
        list.read(&mut data.as_slice(), true, 30, |_| Some(0))
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list.match_id(0), 0);
    }

    #[test]
    fn read_unresolved_name_uses_remote_id() {
        // Create encoded data using the actual encoder for protocol 30
        let mut data = Vec::new();
        crate::write_varint(&mut data, 50).unwrap(); // Use a simpler ID
        data.push(7); // len
        data.extend_from_slice(b"unknown");
        crate::write_varint(&mut data, 0).unwrap(); // terminator

        let mut list = IdList::new();
        list.read(&mut data.as_slice(), false, 30, |_| None)
            .unwrap();
        // Name not resolved, falls back to remote ID
        assert_eq!(list.match_id(50), 50);
    }

    #[test]
    fn write_read_roundtrip_proto30() {
        let mut sender = IdList::new();
        sender.add_id(1, Some(b"root".to_vec()));
        sender.add_id(1000, Some(b"user".to_vec()));

        let mut buf = Vec::new();
        sender.write(&mut buf, false, 30).unwrap();

        let mut receiver = IdList::new();
        receiver
            .read(&mut buf.as_slice(), false, 30, |name| match name {
                b"root" => Some(0),
                b"user" => Some(500),
                _ => None,
            })
            .unwrap();

        assert_eq!(receiver.match_id(1), 0);
        assert_eq!(receiver.match_id(1000), 500);
    }

    #[test]
    fn write_read_roundtrip_proto29() {
        let mut sender = IdList::new();
        sender.add_id(1, Some(b"root".to_vec()));
        sender.add_id(1000, Some(b"user".to_vec()));

        let mut buf = Vec::new();
        sender.write(&mut buf, false, 29).unwrap();

        let mut receiver = IdList::new();
        receiver
            .read(&mut buf.as_slice(), false, 29, |name| match name {
                b"root" => Some(0),
                b"user" => Some(500),
                _ => None,
            })
            .unwrap();

        assert_eq!(receiver.match_id(1), 0);
        assert_eq!(receiver.match_id(1000), 500);
    }

    #[test]
    fn clear_removes_all_entries() {
        let mut list = IdList::new();
        list.add_id(1000, Some(b"testuser".to_vec()));
        list.add_id(1001, Some(b"other".to_vec()));
        assert_eq!(list.len(), 2);

        list.clear();
        assert!(list.is_empty());
    }

    #[test]
    fn add_id_without_name() {
        let mut list = IdList::new();
        list.add_id(1000, None);
        assert_eq!(list.len(), 1);
        // Without a name, nothing is written for this ID
        let mut buf = Vec::new();
        list.write(&mut buf, false, 30).unwrap();
        assert_eq!(buf, vec![0]); // Just terminator
    }

    #[test]
    fn read_with_kind_emits_per_entry_trace() {
        // upstream: uidlist.c:287-291 - "%sid %u(%s) maps to %u" fires for
        // every recv_add_id, including the id=0 entry under ID0_NAMES.
        use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

        let mut cfg = VerbosityConfig::default();
        cfg.debug.own = 2;
        init(cfg);
        let _ = drain_events();

        // wire: varint(1), len(4), "root", varint(0), len(4), "root" (id=0 name)
        let data = vec![1, 4, b'r', b'o', b'o', b't', 0, 4, b'r', b'o', b'o', b't'];
        let mut list = IdList::new();
        list.read_with_kind(&mut data.as_slice(), true, 30, Some(IdKind::Uid), |name| {
            if name == b"root" { Some(0) } else { None }
        })
        .unwrap();

        let messages: Vec<String> = drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Own,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            messages.iter().any(|s| s == "uid 1(root) maps to 0"),
            "missing uid 1 trace: {messages:?}"
        );
        assert!(
            messages.iter().any(|s| s == "uid 0(root) maps to 0"),
            "missing id=0 trace: {messages:?}"
        );
    }

    #[test]
    fn read_with_kind_none_suppresses_trace() {
        // Passing `kind = None` must behave identically to `read` and
        // not emit anything regardless of the configured debug level.
        use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

        let mut cfg = VerbosityConfig::default();
        cfg.debug.own = 2;
        init(cfg);
        let _ = drain_events();

        let data = vec![1, 4, b'r', b'o', b'o', b't', 0];
        let mut list = IdList::new();
        list.read_with_kind(&mut data.as_slice(), false, 30, None, |_| Some(0))
            .unwrap();

        let messages: Vec<String> = drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Own,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            messages.is_empty(),
            "kind=None must suppress OWN trace, got {messages:?}"
        );
    }

    /// Defence-in-depth: ID list exceeding MAX_WIRE_ID_LIST_ENTRIES is rejected.
    #[test]
    fn read_rejects_oversized_id_list() {
        // Build a wire stream with MAX_WIRE_ID_LIST_ENTRIES + 1 entries
        // (each: varint id, len=1, name byte) followed by a terminator.
        let mut data = Vec::new();
        for i in 1..=(MAX_WIRE_ID_LIST_ENTRIES + 1) {
            // Encode id as varint (protocol 30). Use i as id (all unique).
            crate::write_varint(&mut data, i as i32).unwrap();
            data.push(1); // name length
            data.push(b'x'); // name byte
        }
        // Terminator (will never be reached if cap works)
        crate::write_varint(&mut data, 0).unwrap();

        let mut list = IdList::new();
        let result = list.read(&mut data.as_slice(), false, 30, |_| None);
        assert!(result.is_err(), "oversized ID list should be rejected");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds maximum"),
            "error should mention exceeds maximum, got: {err}"
        );
    }

    /// ID list at exactly MAX_WIRE_ID_LIST_ENTRIES should be accepted.
    #[test]
    fn read_accepts_id_list_at_cap() {
        let mut data = Vec::new();
        for i in 1..=MAX_WIRE_ID_LIST_ENTRIES {
            crate::write_varint(&mut data, i as i32).unwrap();
            data.push(1);
            data.push(b'x');
        }
        crate::write_varint(&mut data, 0).unwrap();

        let mut list = IdList::new();
        let result = list.read(&mut data.as_slice(), false, 30, |_| None);
        assert!(
            result.is_ok(),
            "ID list at cap should be accepted, got: {result:?}"
        );
        // HashMap deduplicates, but we inserted unique IDs, so length should match.
        assert_eq!(list.len(), MAX_WIRE_ID_LIST_ENTRIES);
    }
}
