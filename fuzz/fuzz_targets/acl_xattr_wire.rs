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
//! length-prefix arithmetic, so a panic or unbounded allocation here is a
//! finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run acl_xattr_wire
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::acl::read_acl_definition;
use protocol::xattr::{
    XattrList, read_xattr_definitions, recv_xattr, recv_xattr_request, recv_xattr_values,
};

fuzz_target!(|data: &[u8]| {
    // Each decoder consumes a single record; feed the same bytes to all
    // five so libFuzzer can specialise the corpus for each parser shape.
    // The two `recv_xattr_*` decoders mutate a fresh `XattrList` per call
    // so input ordering between calls cannot leak state across parsers.

    let mut acl_cursor = Cursor::new(data);
    let _ = read_acl_definition(&mut acl_cursor);

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
