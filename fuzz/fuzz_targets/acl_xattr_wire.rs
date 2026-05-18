#![no_main]

//! Fuzz target for the authenticated ACL/xattr wire decoders.
//!
//! When `-A` (ACLs) or `-X` (xattrs) is negotiated, the file-list and
//! transfer phases carry literal ACL and xattr definition blocks following
//! a cache-miss index. The two decoders are stateful and varint-heavy:
//!
//! - `protocol::acl::read_acl_definition` parses the literal ACL body
//!   (flags byte + four optional permission varints + named-id list).
//!   Upstream: `acls.c:recv_rsync_acl()` literal-data branch.
//! - `protocol::xattr::read_xattr_definitions` parses the xattr name/value
//!   set (count + per-entry name_len/datum_len varints + NUL-terminated
//!   names + values or 16-byte MD5 checksums for abbreviated entries).
//!   Upstream: `xattrs.c:receive_xattr()`.
//!
//! Both are reached from an authenticated peer and contain length-prefix
//! arithmetic, so a panic-or-OOM here is a finding.
//!
//! # Audit substitution
//!
//! The audit suggested `RsyncAcl::parse`, `parse_xattr`, and
//! `XattrEntry::decode`. Those names don't exist as public APIs. The closest
//! authenticated bytes-in decoders are `read_acl_definition` and
//! `read_xattr_definitions` in the `protocol` crate, which is what we fuzz.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run acl_xattr_wire
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::acl::read_acl_definition;
use protocol::xattr::read_xattr_definitions;

fuzz_target!(|data: &[u8]| {
    // Each decoder consumes a single record; feed the same bytes to both
    // so libFuzzer can specialise the corpus for each parser shape.
    let mut acl_cursor = Cursor::new(data);
    let _ = read_acl_definition(&mut acl_cursor);

    let mut xattr_cursor = Cursor::new(data);
    let _ = read_xattr_definitions(&mut xattr_cursor);
});
