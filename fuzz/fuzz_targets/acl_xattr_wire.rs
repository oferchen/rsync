#![no_main]

//! Fuzz target for the authenticated ACL/xattr wire decoders.
//!
//! When `-A` (ACLs) or `-X` (xattrs) is negotiated, the file-list and
//! transfer phases carry literal ACL and xattr definition blocks following
//! a cache-miss index, plus negotiated request/value batches. The decoders
//! are stateful and varint-heavy:
//!
//! - `protocol::acl::read_acl_definition` parses the literal ACL body
//!   (flags byte + four optional permission varints + named-id list).
//!   Upstream: `acls.c:recv_rsync_acl()` literal-data branch.
//! - `protocol::xattr::read_xattr_definitions` parses the xattr name/value
//!   set (count + per-entry name_len/datum_len varints + NUL-terminated
//!   names + values or 16-byte MD5 checksums for abbreviated entries).
//!   Upstream: `xattrs.c:receive_xattr()`.
//! - `protocol::xattr::recv_xattr` reads one cached-or-literal definition
//!   record. Upstream: `xattrs.c:recv_xattr()`.
//! - `protocol::xattr::recv_xattr_request` reads the delta-encoded list of
//!   1-based indices the receiver is asking the sender to inline.
//!   Upstream: `xattrs.c:recv_xattr_request()`.
//! - `protocol::xattr::recv_xattr_values` reads the per-index value bodies
//!   that satisfy a prior request. Upstream: `xattrs.c:recv_xattr_values()`.
//!
//! All five entry points are reached from an authenticated peer and contain
//! length-prefix arithmetic, so a panic or unbounded allocation is a finding.
//!
//! # Oracle
//!
//! The ACL body has a matching public encoder, so this target holds it to an
//! encode/decode fixpoint: a decoded `AclDefinition` is re-encoded, decoded
//! again (a decode failure on the encoder's own output is a finding, not a
//! silent return), and re-encoded; the two encodings must be byte-identical.
//! Comparing *encodings* rather than the structs sidesteps the deliberate
//! non-injectivity of the decoder (it injects a computed mask entry and
//! reorders standard entries into fixed slots), while still catching any
//! field the encode/decode pair mishandles.
//!
//! The four xattr decoders are exercised for panic-freedom only. They are
//! pure tolerant parsers with no reachable inverse on this surface: their
//! encoders consume an `XattrList` seeded with negotiated cache/request
//! state that a raw fuzz buffer does not reconstruct (e.g. `recv_xattr_request`
//! filters every index against an empty list, so its output is always
//! empty), and `read_xattr_definitions` returns an `XattrSet` with no
//! set-level encoder. No-panic is therefore the only sound oracle here.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run acl_xattr_wire
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::acl::{read_acl_definition, write_acl_definition};
use protocol::xattr::{
    XattrList, read_xattr_definitions, recv_xattr, recv_xattr_request, recv_xattr_values,
};

fuzz_target!(|data: &[u8]| {
    // ACL body: decode, then assert the encoder/decoder pair is a byte-level
    // fixpoint on the decoded value.
    let mut acl_cursor = Cursor::new(data);
    if let Ok(definition) = read_acl_definition(&mut acl_cursor) {
        let mut first = Vec::new();
        write_acl_definition(&mut first, &definition)
            .expect("encoding a parsed ACL into a Vec cannot fail");

        let mut first_cursor = Cursor::new(first.as_slice());
        let redecoded = read_acl_definition(&mut first_cursor)
            .expect("re-decoding a self-encoded ACL definition must succeed");

        let mut second = Vec::new();
        write_acl_definition(&mut second, &redecoded)
            .expect("re-encoding a re-decoded ACL cannot fail");

        assert_eq!(first, second, "ACL definition encoding is not a fixpoint",);
    }

    // The remaining decoders are tolerant parsers with no reachable inverse
    // on this surface; drive each on a fresh cursor for panic-freedom only.
    let mut definitions_cursor = Cursor::new(data);
    let _ = read_xattr_definitions(&mut definitions_cursor);

    let mut recv_cursor = Cursor::new(data);
    let _ = recv_xattr(&mut recv_cursor);

    let mut request_cursor = Cursor::new(data);
    let mut request_list = XattrList::new();
    let _ = recv_xattr_request(&mut request_cursor, &mut request_list);

    let mut values_cursor = Cursor::new(data);
    let mut values_list = XattrList::new();
    let _ = recv_xattr_values(&mut values_cursor, &mut values_list);
});
