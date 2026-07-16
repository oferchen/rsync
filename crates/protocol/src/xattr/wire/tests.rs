use std::io::Cursor;

use crate::varint::write_varint;
use crate::xattr::{MAX_FULL_DATUM, MAX_XATTR_DIGEST_LEN, XattrEntry, XattrList};

use super::encode::compute_xattr_checksum;
use super::types::RecvXattrResult;
use super::{
    XattrDefinition, checksum_matches, read_xattr_definitions, recv_xattr, recv_xattr_request,
    recv_xattr_values, send_sender_xattr_response, send_xattr, send_xattr_request,
    send_xattr_values,
};
use crate::varint::read_varint;
use crate::xattr::XattrState;

/// Writes a literal xattr definition block (count + entries) in wire format.
///
/// Names are NUL-terminated on the wire. Each entry has name_len (including NUL),
/// datum_len, name bytes, and value or fake checksum bytes.
fn write_definition_block(buf: &mut Vec<u8>, entries: &[(&[u8], &[u8])]) {
    write_varint(buf, entries.len() as i32).unwrap();
    for &(name, value) in entries {
        write_varint(buf, (name.len() + 1) as i32).unwrap();
        write_varint(buf, value.len() as i32).unwrap();
        buf.extend_from_slice(name);
        buf.push(0);
        if value.len() > MAX_FULL_DATUM {
            buf.extend_from_slice(&[0xAA; MAX_XATTR_DIGEST_LEN]);
        } else {
            buf.extend_from_slice(value);
        }
    }
}

#[test]
fn read_definitions_empty() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
}

#[test]
fn read_definitions_single_small_entry() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.test", b"hello")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 1);
    let entry = &set.entries()[0];
    assert_eq!(entry.name(), b"user.test");
    assert_eq!(entry.datum(), b"hello");
    assert_eq!(entry.datum_len(), 5);
    assert!(!entry.is_abbreviated());
}

#[test]
fn read_definitions_multiple_entries() {
    let mut buf = Vec::new();
    write_definition_block(
        &mut buf,
        &[
            (b"user.alpha", b"value_a"),
            (b"user.beta", b"value_b"),
            (b"user.gamma", b"value_c"),
        ],
    );

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 3);
    assert_eq!(set.entries()[0].name(), b"user.alpha");
    assert_eq!(set.entries()[0].datum(), b"value_a");
    assert_eq!(set.entries()[1].name(), b"user.beta");
    assert_eq!(set.entries()[1].datum(), b"value_b");
    assert_eq!(set.entries()[2].name(), b"user.gamma");
    assert_eq!(set.entries()[2].datum(), b"value_c");
}

#[test]
fn read_definitions_abbreviated_large_value() {
    let large_value = vec![0xBB; 100];
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.large", &large_value)]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 1);
    let entry = &set.entries()[0];
    assert_eq!(entry.name(), b"user.large");
    assert!(entry.is_abbreviated());
    assert_eq!(entry.datum_len(), 100);
    assert_eq!(entry.datum().len(), MAX_XATTR_DIGEST_LEN);
}

#[test]
fn read_definitions_mixed_small_and_large() {
    let large_value = vec![0xCC; 64];
    let mut buf = Vec::new();
    write_definition_block(
        &mut buf,
        &[
            (b"user.small", b"tiny"),
            (b"user.large", &large_value),
            (b"user.also_small", b"also tiny"),
        ],
    );

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 3);
    assert!(!set.entries()[0].is_abbreviated());
    assert_eq!(set.entries()[0].datum(), b"tiny");
    assert!(set.entries()[1].is_abbreviated());
    assert_eq!(set.entries()[1].datum_len(), 64);
    assert!(!set.entries()[2].is_abbreviated());
    assert_eq!(set.entries()[2].datum(), b"also tiny");
}

#[test]
fn read_definitions_empty_value() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.empty", b"")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 1);
    assert!(!set.entries()[0].is_abbreviated());
    assert!(set.entries()[0].datum().is_empty());
    assert_eq!(set.entries()[0].datum_len(), 0);
}

#[test]
fn read_definitions_boundary_value() {
    let boundary_value = vec![0x42u8; MAX_FULL_DATUM];
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.boundary", &boundary_value)]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 1);
    assert!(!set.entries()[0].is_abbreviated());
    assert_eq!(set.entries()[0].datum(), &boundary_value);
}

#[test]
fn read_definitions_one_over_boundary() {
    let over_value = vec![0x42u8; MAX_FULL_DATUM + 1];
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.over", &over_value)]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.len(), 1);
    assert!(set.entries()[0].is_abbreviated());
    assert_eq!(set.entries()[0].datum_len(), MAX_FULL_DATUM + 1);
}

#[test]
fn read_definitions_binary_value() {
    let binary_value: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x00];
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.bin", &binary_value)]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();

    assert_eq!(set.entries()[0].datum(), &binary_value);
}

#[test]
fn read_definitions_negative_count_fails() {
    let mut buf = Vec::new();
    write_varint(&mut buf, -1).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("negative xattr count"));
}

#[test]
fn read_definitions_missing_nul_fails() {
    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, 4).unwrap();
    write_varint(&mut buf, 1).unwrap();
    buf.extend_from_slice(b"test");
    buf.push(0x42);

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("NUL"));
}

#[test]
fn read_definitions_empty_name_fails() {
    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, 0).unwrap();
    write_varint(&mut buf, 1).unwrap();
    buf.push(0x42);

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn definition_name_lossy() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.test", b"val")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    assert_eq!(set.entries()[0].name_lossy(), "user.test");
}

#[test]
fn xattr_set_into_xattr_list() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.a", b"val_a"), (b"user.b", b"val_b")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let list = set.into_xattr_list();

    assert_eq!(list.len(), 2);
    assert_eq!(list.entries()[0].name(), b"user.a");
    assert_eq!(list.entries()[0].datum(), b"val_a");
    assert_eq!(list.entries()[1].name(), b"user.b");
    assert_eq!(list.entries()[1].datum(), b"val_b");
}

#[test]
fn xattr_set_into_xattr_list_with_abbreviated() {
    let large_value = vec![0xDD; 50];
    let mut buf = Vec::new();
    write_definition_block(
        &mut buf,
        &[(b"user.small", b"tiny"), (b"user.large", &large_value)],
    );

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let list = set.into_xattr_list();

    assert_eq!(list.len(), 2);
    assert!(!list.entries()[0].is_abbreviated());
    assert!(list.entries()[1].is_abbreviated());
    assert_eq!(list.entries()[1].datum_len(), 50);
}

#[test]
fn definition_into_entry_full() {
    let def = XattrDefinition {
        name: b"user.test".to_vec(),
        datum: b"value".to_vec(),
        datum_len: 5,
        abbreviated: false,
    };

    let entry = def.into_entry();
    assert_eq!(entry.name(), b"user.test");
    assert_eq!(entry.datum(), b"value");
    assert!(!entry.is_abbreviated());
}

#[test]
fn definition_into_entry_abbreviated() {
    let def = XattrDefinition {
        name: b"user.large".to_vec(),
        datum: vec![0xAA; MAX_XATTR_DIGEST_LEN],
        datum_len: 100,
        abbreviated: true,
    };

    let entry = def.into_entry();
    assert_eq!(entry.name(), b"user.large");
    assert!(entry.is_abbreviated());
    assert_eq!(entry.datum_len(), 100);
    assert_eq!(entry.datum().len(), MAX_XATTR_DIGEST_LEN);
}

#[test]
fn xattr_set_into_entries() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.x", b"x_val")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let entries = set.into_entries();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), b"user.x");
}

#[test]
fn xattr_set_iter() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.a", b"1"), (b"user.b", b"2")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let names: Vec<&[u8]> = set.iter().map(|d| d.name()).collect();
    assert_eq!(names, vec![b"user.a".as_slice(), b"user.b".as_slice()]);
}

#[test]
fn xattr_set_into_iterator() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.x", b"val")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let collected: Vec<XattrDefinition> = set.into_iter().collect();
    assert_eq!(collected.len(), 1);
}

#[test]
fn xattr_set_ref_into_iterator() {
    let mut buf = Vec::new();
    write_definition_block(&mut buf, &[(b"user.x", b"val")]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    let names: Vec<&[u8]> = (&set).into_iter().map(|d| d.name()).collect();
    assert_eq!(names, vec![b"user.x".as_slice()]);
}

#[test]
fn read_definitions_many_entries() {
    let mut entries_data: Vec<(&[u8], Vec<u8>)> = Vec::new();
    for i in 0..20u8 {
        let name = format!("user.attr_{i:02}");
        let value = format!("value_{i:02}");
        entries_data.push((
            Box::leak(name.into_bytes().into_boxed_slice()),
            value.into_bytes(),
        ));
    }
    let refs: Vec<(&[u8], &[u8])> = entries_data
        .iter()
        .map(|(n, v)| (*n, v.as_slice()))
        .collect();

    let mut buf = Vec::new();
    write_definition_block(&mut buf, &refs);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).unwrap();
    assert_eq!(set.len(), 20);
    assert_eq!(set.entries()[0].name(), b"user.attr_00");
    assert_eq!(set.entries()[19].name(), b"user.attr_19");
}

#[test]
fn round_trip_small_xattrs() {
    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.test".to_vec(),
        b"small value".to_vec(),
    ));
    list.push(XattrEntry::new(b"user.other".to_vec(), b"another".to_vec()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 2);
            assert_eq!(received.entries()[0].name(), b"user.test");
            assert_eq!(received.entries()[0].datum(), b"small value");
            assert!(!received.entries()[0].is_abbreviated());
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_large_xattr_abbreviated() {
    let large_value = vec![0xABu8; 100];
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.large".to_vec(), large_value.clone()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert!(received.entries()[0].is_abbreviated());
            assert_eq!(received.entries()[0].datum_len(), 100);
            assert!(checksum_matches(
                received.entries()[0].datum(),
                &large_value,
                0
            ));
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn cache_hit_sends_index_only() {
    let list = XattrList::new();

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, Some(42), 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::CacheHit(idx) => assert_eq!(idx, 42),
        _ => panic!("Expected cache hit"),
    }
}

#[test]
fn checksum_verification() {
    let seed = 12345;
    let value = b"test value for checksum";
    let checksum = compute_xattr_checksum(value, seed);

    assert!(checksum_matches(&checksum, value, seed));
    assert!(!checksum_matches(&checksum, b"different value", seed));
}

/// The abbreviation digest of protocol 30-32 is the plain, unseeded MD5 of the
/// value. Upstream computes it via `sum_init(xattr_sum_nni, checksum_seed)`,
/// but the `CSUM_MD5` case of `sum_init()` does `md5_begin()` with no seed
/// (`checksum.c:588`); only the MD4-family cases fold the seed in. A seeded
/// digest here would never equal the receiver's locally computed value, so
/// upstream would re-request every large xattr in full. This locks the digest
/// to seed-independent plain MD5 so oc-rsync stays byte-compatible.
#[test]
fn abbreviation_checksum_is_unseeded_md5() {
    use md5::{Digest, Md5};

    let value = b"same data different seeds";

    // The seed must not change the digest.
    let checksum_a = compute_xattr_checksum(value, 100);
    let checksum_b = compute_xattr_checksum(value, 200);
    let checksum_zero = compute_xattr_checksum(value, 0);
    assert_eq!(checksum_a, checksum_b);
    assert_eq!(checksum_a, checksum_zero);

    // The digest is exactly the plain MD5 of the value, matching upstream's
    // unseeded sum_init(CSUM_MD5) over the datum.
    let expected: [u8; MAX_XATTR_DIGEST_LEN] = Md5::digest(value).into();
    assert_eq!(checksum_a, expected);

    // Any nonzero seed still verifies, since the seed is ignored.
    assert!(checksum_matches(&checksum_a, value, 200));
    assert!(!checksum_matches(&checksum_a, b"different value", 200));
}

#[test]
fn round_trip_empty_xattr_list() {
    let list = XattrList::new();

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 0);
            assert!(received.is_empty());
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_empty_xattr_value() {
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.empty".to_vec(), b"".to_vec()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert_eq!(received.entries()[0].name(), b"user.empty");
            assert!(received.entries()[0].datum().is_empty());
            assert!(!received.entries()[0].is_abbreviated());
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_xattr_at_abbreviation_boundary() {
    let value_at_boundary = vec![0x42u8; MAX_FULL_DATUM];
    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.boundary".to_vec(),
        value_at_boundary.clone(),
    ));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert!(!received.entries()[0].is_abbreviated());
            assert_eq!(received.entries()[0].datum(), &value_at_boundary);
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_xattr_one_byte_over_boundary() {
    let value_over_boundary = vec![0x42u8; MAX_FULL_DATUM + 1];
    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.over_boundary".to_vec(),
        value_over_boundary.clone(),
    ));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert!(received.entries()[0].is_abbreviated());
            assert_eq!(received.entries()[0].datum_len(), MAX_FULL_DATUM + 1);
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_many_xattrs() {
    let mut list = XattrList::new();
    for i in 0..20 {
        let name = format!("user.attr_{i:02}");
        let value = format!("value_{i:02}");
        list.push(XattrEntry::new(name.into_bytes(), value.into_bytes()));
    }

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 20);
            for i in 0..20 {
                let expected_name = format!("user.attr_{i:02}");
                let expected_value = format!("value_{i:02}");
                assert_eq!(received.entries()[i].name(), expected_name.as_bytes());
                assert_eq!(received.entries()[i].datum(), expected_value.as_bytes());
            }
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_mixed_small_and_large_xattrs() {
    let small_value = b"small".to_vec();
    let large_value = vec![0xCDu8; 100];

    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.small1".to_vec(),
        small_value.clone(),
    ));
    list.push(XattrEntry::new(
        b"user.large1".to_vec(),
        large_value.clone(),
    ));
    list.push(XattrEntry::new(
        b"user.small2".to_vec(),
        small_value.clone(),
    ));
    list.push(XattrEntry::new(
        b"user.large2".to_vec(),
        large_value.clone(),
    ));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 4);
            assert!(!received.entries()[0].is_abbreviated());
            assert_eq!(received.entries()[0].datum(), &small_value);
            assert!(received.entries()[1].is_abbreviated());
            assert!(checksum_matches(
                received.entries()[1].datum(),
                &large_value,
                0
            ));
            assert!(!received.entries()[2].is_abbreviated());
            assert!(received.entries()[3].is_abbreviated());
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_binary_xattr_value() {
    let binary_value: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x00, 0xAB, 0xCD, 0x00];
    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.binary".to_vec(),
        binary_value.clone(),
    ));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert_eq!(received.entries()[0].datum(), &binary_value);
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_utf8_xattr_value() {
    let utf8_value = "Hello \u{4e16}\u{754c}!".as_bytes().to_vec();
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.utf8".to_vec(), utf8_value.clone()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert!(!received.entries()[0].is_abbreviated());
            assert_eq!(received.entries()[0].datum(), &utf8_value);
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn round_trip_large_utf8_xattr_value_abbreviated() {
    let utf8_value = "Hello, \u{4e16}\u{754c}! \u{1f30d} \u{41f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}!".as_bytes().to_vec();
    assert!(utf8_value.len() > MAX_FULL_DATUM);

    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"user.utf8_large".to_vec(),
        utf8_value.clone(),
    ));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::Literal(received) => {
            assert_eq!(received.len(), 1);
            assert!(received.entries()[0].is_abbreviated());
            assert!(checksum_matches(
                received.entries()[0].datum(),
                &utf8_value,
                0
            ));
        }
        _ => panic!("Expected literal data"),
    }
}

#[test]
fn xattr_request_round_trip() {
    let indices = vec![0, 1, 3, 5, 10];

    let mut buf = Vec::new();
    send_xattr_request(&mut buf, &indices).unwrap();

    let mut list = XattrList::new();
    for i in 0..=10 {
        list.push(XattrEntry::abbreviated(
            format!("user.attr{i}").into_bytes(),
            vec![0u8; MAX_XATTR_DIGEST_LEN],
            100,
        ));
    }

    let mut cursor = Cursor::new(buf);
    let received_indices = recv_xattr_request(&mut cursor, &mut list).unwrap();

    assert_eq!(received_indices, indices);
    assert!(list.entries()[0].state().needs_send());
    assert!(list.entries()[1].state().needs_send());
    assert!(!list.entries()[2].state().needs_send());
    assert!(list.entries()[3].state().needs_send());
    assert!(list.entries()[5].state().needs_send());
    assert!(list.entries()[10].state().needs_send());
}

#[test]
fn xattr_request_empty() {
    let indices: Vec<usize> = vec![];

    let mut buf = Vec::new();
    send_xattr_request(&mut buf, &indices).unwrap();

    let mut list = XattrList::new();
    list.push(XattrEntry::abbreviated(
        b"user.test".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        100,
    ));

    let mut cursor = Cursor::new(buf);
    let received_indices = recv_xattr_request(&mut cursor, &mut list).unwrap();

    assert!(received_indices.is_empty());
    assert!(!list.entries()[0].state().needs_send());
}

#[test]
fn xattr_values_round_trip() {
    let value1 = vec![1u8; 50];
    let value2 = vec![2u8; 75];

    let mut sender_list = XattrList::new();
    sender_list.push(XattrEntry::new(b"user.attr1".to_vec(), value1.clone()));
    sender_list.push(XattrEntry::new(b"user.attr2".to_vec(), value2.clone()));
    sender_list.entries_mut()[0].mark_todo();
    sender_list.entries_mut()[1].mark_todo();

    let mut buf = Vec::new();
    send_xattr_values(&mut buf, &sender_list).unwrap();

    let mut receiver_list = XattrList::new();
    receiver_list.push(XattrEntry::abbreviated(
        b"user.attr1".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        50,
    ));
    receiver_list.push(XattrEntry::abbreviated(
        b"user.attr2".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        75,
    ));

    let mut cursor = Cursor::new(buf);
    recv_xattr_values(&mut cursor, &mut receiver_list).unwrap();

    assert_eq!(receiver_list.entries()[0].datum(), &value1);
    assert_eq!(receiver_list.entries()[1].datum(), &value2);
    assert!(!receiver_list.entries()[0].is_abbreviated());
    assert!(!receiver_list.entries()[1].is_abbreviated());
}

#[test]
fn checksum_matches_empty_value() {
    let empty_value = b"";
    let checksum = compute_xattr_checksum(empty_value, 0);
    assert!(checksum_matches(&checksum, empty_value, 0));
}

/// Verifies the sender-side response writes a single 0 terminator when no
/// entries are marked TODO. This is the byte stream upstream's
/// `send_xattr_request(fname, file, f_out)` emits when the generator set
/// `ITEM_REPORT_XATTR` only because of a count mismatch and no entries
/// require their full value to be transferred. Dropping this terminator
/// desyncs the wire stream under `-X --fake-super`.
#[test]
fn sender_response_emits_terminator_when_no_todo() {
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.small".to_vec(), b"value".to_vec()));
    list.entries_mut()[0].set_num(1);

    let mut buf = Vec::new();
    send_sender_xattr_response(&mut buf, &mut list).unwrap();

    assert_eq!(buf, vec![0u8]);
}

/// Verifies the sender-side response emits delta-encoded num + length + value
/// for each XSTATE_TODO entry, then a 0 terminator. Matches upstream
/// `xattrs.c:send_xattr_request()` sender path (`fname != NULL`,
/// `f_out >= 0`).
#[test]
fn sender_response_round_trip_with_todo_entries() {
    let value_a = vec![0xAAu8; 80];
    let value_b = vec![0xBBu8; 120];

    let mut sender_list = XattrList::new();
    let mut entry_a = XattrEntry::new(b"user.large_a".to_vec(), value_a.clone());
    entry_a.set_num(1);
    entry_a.mark_todo();
    sender_list.push(entry_a);

    let mut middle = XattrEntry::new(b"user.middle".to_vec(), vec![0u8; 10]);
    middle.set_num(2);
    sender_list.push(middle);

    let mut entry_b = XattrEntry::new(b"user.large_b".to_vec(), value_b.clone());
    entry_b.set_num(3);
    entry_b.mark_todo();
    sender_list.push(entry_b);

    let mut buf = Vec::new();
    send_sender_xattr_response(&mut buf, &mut sender_list).unwrap();

    let mut cursor = Cursor::new(buf);
    let rel1 = read_varint(&mut cursor).unwrap();
    let len1 = read_varint(&mut cursor).unwrap();
    let mut value1 = vec![0u8; len1 as usize];
    use std::io::Read;
    cursor.read_exact(&mut value1).unwrap();

    let rel2 = read_varint(&mut cursor).unwrap();
    let len2 = read_varint(&mut cursor).unwrap();
    let mut value2 = vec![0u8; len2 as usize];
    cursor.read_exact(&mut value2).unwrap();

    let terminator = read_varint(&mut cursor).unwrap();

    assert_eq!(rel1, 1);
    assert_eq!(len1, 80);
    assert_eq!(value1, value_a);
    assert_eq!(rel2, 2);
    assert_eq!(len2, 120);
    assert_eq!(value2, value_b);
    assert_eq!(terminator, 0);

    assert_eq!(sender_list.entries()[0].state(), XattrState::Done);
    assert_eq!(sender_list.entries()[1].state(), XattrState::Done);
    assert_eq!(sender_list.entries()[2].state(), XattrState::Done);
}

#[test]
fn checksum_length_mismatch_returns_false() {
    let value = b"test value";
    let short_checksum = &[0u8; 8];
    assert!(!checksum_matches(short_checksum, value, 0));
}

#[test]
fn cache_index_zero() {
    let list = XattrList::new();

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, Some(0), 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::CacheHit(idx) => assert_eq!(idx, 0),
        _ => panic!("Expected cache hit"),
    }
}

#[test]
fn large_cache_index() {
    let list = XattrList::new();
    let large_index = 100_000u32;

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, Some(large_index), 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor).unwrap();

    match result {
        RecvXattrResult::CacheHit(idx) => assert_eq!(idx, large_index),
        _ => panic!("Expected cache hit"),
    }
}

/// BR-3h regression (#2494): the sender must emit `user.foo` xattr names
/// verbatim on the wire. Prior behaviour stripped the `user.` prefix on
/// Linux, causing the destination to land the entry in the wrong
/// namespace (`user.rsync.foo` under fake-super or a silent drop without
/// it) when interoperating with upstream rsync 3.4.1.
///
/// Asserts that:
///
/// 1. `protocol::xattr::local_to_wire` produces the upstream-faithful wire
///    name on the host platform. On Linux the local name `user.foo` is
///    written byte-for-byte; on non-Linux the local flat-namespace name
///    `foo` is wrapped with the `user.` prefix the sender prepends before
///    handing bytes to `send_xattr`. Either way the resulting wire bytes
///    must equal `user.foo`.
/// 2. The bytes `b"user.foo"` appear in the serialized `send_xattr`
///    output as the on-wire name, immediately followed by the NUL
///    terminator. This is a byte-for-byte parity check against upstream
///    rsync 3.4.1 `xattrs.c:send_xattr()`.
#[test]
fn user_prefix_preserved_in_wire_bytes() {
    use crate::xattr::{XattrEntry, XattrList, local_to_wire};

    // Step 1: local_to_wire must yield the upstream wire name `user.foo`
    // on every supported platform, regardless of `am_root`. Linux feeds
    // it the verbatim `user.foo` name; non-Linux feeds the flat-namespace
    // `foo` so the sender's prefix wrapper produces the same wire bytes.
    #[cfg(target_os = "linux")]
    let local_name: &[u8] = b"user.foo";
    #[cfg(not(target_os = "linux"))]
    let local_name: &[u8] = b"foo";

    for am_root in [false, true] {
        assert_eq!(
            local_to_wire(local_name, am_root),
            Some(b"user.foo".to_vec()),
            "local_to_wire must emit `user.foo` on the wire (am_root={am_root})",
        );
    }

    // Step 2: an XattrEntry built from the wire name must serialize the
    // bytes `user.foo` verbatim into the wire stream.
    let wire_name = local_to_wire(local_name, false).expect("name must not be filtered");
    let mut list = XattrList::new();
    list.push(XattrEntry::new(wire_name, b"bar".to_vec()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    // Locate the name bytes. The wire layout is:
    //   varint(ndx+1=0) | varint(count=1) | varint(name_len=9) |
    //   varint(datum_len=3) | "user.foo\0" | "bar"
    let needle = b"user.foo\0";
    let position = buf
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("expected `user.foo\\0` in wire bytes; got {buf:?}"));
    // The stripped form (BR-3h regression) would expose `foo\0` instead.
    assert!(
        !buf.windows(4).any(|w| w == b"foo\0") || buf[position + 5..].starts_with(b"foo\0"),
        "regression: wire bytes contain a bare `foo\\0` token; the \
         `user.` prefix was stripped (BR-3h #2494)",
    );
    // Payload follows the name + NUL terminator.
    assert_eq!(&buf[position + needle.len()..], b"bar");
}

#[test]
fn read_definitions_exceeds_max_count() {
    use crate::xattr::MAX_WIRE_XATTR_COUNT;

    let mut buf = Vec::new();
    write_varint(&mut buf, (MAX_WIRE_XATTR_COUNT as i32) + 1).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn read_definitions_exceeds_max_name_len() {
    use crate::xattr::MAX_WIRE_XATTR_NAME_LEN;

    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, (MAX_WIRE_XATTR_NAME_LEN as i32) + 1).unwrap();
    write_varint(&mut buf, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn read_definitions_exceeds_max_value_len() {
    use crate::max_alloc::effective_max_alloc;

    // Key off the effective --max-alloc (default DEFAULT_MAX_ALLOC = 1 GiB),
    // not a hard-coded constant, so this stays correct when the ceiling moves.
    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, 5).unwrap();
    write_varint(&mut buf, (effective_max_alloc() + 1) as i32).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn recv_xattr_exceeds_max_count() {
    use crate::xattr::MAX_WIRE_XATTR_COUNT;

    let mut buf = Vec::new();
    // ndx + 1 = 0 means ndx = -1, so literal path
    write_varint(&mut buf, 0).unwrap();
    write_varint(&mut buf, (MAX_WIRE_XATTR_COUNT as i32) + 1).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn recv_xattr_exceeds_max_name_len() {
    use crate::xattr::MAX_WIRE_XATTR_NAME_LEN;

    let mut buf = Vec::new();
    write_varint(&mut buf, 0).unwrap();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, (MAX_WIRE_XATTR_NAME_LEN as i32) + 1).unwrap();
    write_varint(&mut buf, 0).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn recv_xattr_exceeds_max_value_len() {
    use crate::max_alloc::effective_max_alloc;

    let mut buf = Vec::new();
    write_varint(&mut buf, 0).unwrap();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, 5).unwrap();
    write_varint(&mut buf, (effective_max_alloc() + 1) as i32).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn recv_xattr_values_exceeds_max_value_len() {
    use crate::max_alloc::effective_max_alloc;

    let mut list = XattrList::new();
    list.push(XattrEntry::abbreviated(
        b"user.test".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        100,
    ));

    let mut buf = Vec::new();
    write_varint(&mut buf, (effective_max_alloc() + 1) as i32).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr_values(&mut cursor, &mut list);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    // WHY: upstream reads these counts/lengths via read_varint_bounded /
    // read_varint_size (xattrs.c:793,802-803; io.c:1904-1926), which
    // exit_cleanup(RERR_PROTOCOL) (exit 2) on overrun. The ProtocolViolation
    // tag must survive so the core exit-code mapper reproduces exit 2, not the
    // RERR_STREAMIO (12) a bare InvalidData maps to - a drop-in tool must
    // distinguish a hostile wire value from a genuinely broken stream.
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr wire value must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

#[test]
fn read_definitions_missing_nul_maps_to_fileio() {
    // A literal xattr name whose declared bytes lack the trailing NUL. upstream:
    // xattrs.c:811-814 receive_xattr() rejects this with
    // exit_cleanup(RERR_FILEIO) (exit 11) - a file-IO error, NOT a protocol (2)
    // or stream (12) error. WHY it matters: a drop-in tool must reproduce
    // upstream's exact exit classification. io::Error::other maps to RERR_FILEIO
    // via the core mapper's catch-all, and the error must NOT carry the
    // ProtocolViolation tag (which would wrongly downgrade the exit code to 2).
    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap(); // count
    write_varint(&mut buf, 3).unwrap(); // name_len (the 3 bytes hold no NUL)
    write_varint(&mut buf, 0).unwrap(); // datum_len
    buf.extend_from_slice(b"abc");

    let mut cursor = Cursor::new(buf);
    let err = read_xattr_definitions(&mut cursor).expect_err("missing NUL must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(
        err.get_ref()
            .is_none_or(|e| !e.is::<crate::protocol_violation::ProtocolViolation>()),
        "missing-NUL must map to RERR_FILEIO (11), never ProtocolViolation (2)"
    );
}

#[test]
fn read_definitions_large_datum_within_max_alloc_decodes() {
    use crate::max_alloc::effective_max_alloc;

    // A macOS resource fork (com.apple.ResourceFork) between the former 64 MiB
    // cap and the effective --max-alloc ceiling (default 1 GiB). Declared
    // datum_len exceeds MAX_FULL_DATUM (32), so this list-phase entry is
    // abbreviated: only the 16-byte digest is on the wire, no large allocation
    // occurs here.
    let datum_len: i32 = 65 * 1024 * 1024; // > old 64 MiB cap, < default cap
    assert!(datum_len as usize <= effective_max_alloc());

    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap(); // count
    write_varint(&mut buf, 10).unwrap(); // name_len (incl NUL)
    write_varint(&mut buf, datum_len).unwrap();
    buf.extend_from_slice(b"user.fork");
    buf.push(0);
    buf.extend_from_slice(&[0xAA; MAX_XATTR_DIGEST_LEN]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).expect("datum below --max-alloc must decode");
    // WHY: upstream reads datum_len via read_varint_size(f, MAX_WIRE_XATTR_DATALEN,
    // ...) at xattrs.c:803, where MAX_WIRE_XATTR_DATALEN is 0x7fffffff (rsync.h:178);
    // the real bound is --max-alloc (default 1 GiB, options.c:203). A 64 MiB cap
    // rejected legitimate resource forks that upstream 3.4.4 accepts under its
    // default config. A drop-in tool must accept the same transfers upstream does.
    assert_eq!(set.len(), 1);
    let entry = &set.entries()[0];
    assert_eq!(entry.name(), b"user.fork");
    assert!(entry.is_abbreviated());
    assert_eq!(entry.datum_len(), datum_len as usize);
}

#[test]
fn read_definitions_datum_over_max_alloc_rejected() {
    use crate::max_alloc::effective_max_alloc;

    // One byte past the effective --max-alloc ceiling stays rejected: the cap is
    // a real allocation bound, not merely the 0x7fffffff wire maximum, so a
    // hostile ~2 GiB claim cannot OOM the receiver.
    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap();
    write_varint(&mut buf, 10).unwrap();
    write_varint(&mut buf, (effective_max_alloc() + 1) as i32).unwrap();
    buf.extend_from_slice(b"user.fork");
    buf.push(0);

    let mut cursor = Cursor::new(buf);
    let err = read_xattr_definitions(&mut cursor).expect_err("datum over cap must be rejected");
    // WHY: upstream's new_array()/my_alloc() aborts a transfer whose single datum
    // exceeds --max-alloc. We hold the same allocation bound and tag it
    // ProtocolViolation so the core mapper yields RERR_PROTOCOL (2), not the
    // RERR_STREAMIO (12) a bare InvalidData maps to.
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr datum must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

/// A raised `--max-alloc` lets the list-phase decoder accept a datum between the
/// former 1 GiB default cap and the `0x7fffffff` field ceiling. The entry is
/// abbreviated (datum_len > MAX_FULL_DATUM), so only the 16-byte digest is on
/// the wire and no multi-GiB allocation occurs. WHY: upstream bounds the datum
/// by the negotiated `--max-alloc` (util2.c:75), not by a fixed per-field cap,
/// so a peer that raised `--max-alloc` may legitimately send this.
#[test]
fn read_definitions_datum_above_default_accepted_when_max_alloc_raised() {
    use crate::max_alloc::{DEFAULT_MAX_ALLOC, effective_max_alloc, set_max_alloc};

    let restore = effective_max_alloc();
    // Raise the ceiling to just below the field maximum, then declare a datum
    // above the 1 GiB default but below the raised ceiling.
    let raised = 0x7000_0000usize; // 1.75 GiB, < 0x7fffffff field ceiling
    let datum_len: i32 = 0x5000_0000; // 1.25 GiB, > DEFAULT_MAX_ALLOC (1 GiB)
    assert!(datum_len as usize > DEFAULT_MAX_ALLOC);
    assert!((datum_len as usize) < raised);
    set_max_alloc(raised);

    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap(); // count
    write_varint(&mut buf, 10).unwrap(); // name_len (incl NUL)
    write_varint(&mut buf, datum_len).unwrap();
    buf.extend_from_slice(b"user.fork");
    buf.push(0);
    buf.extend_from_slice(&[0xAA; MAX_XATTR_DIGEST_LEN]);

    let mut cursor = Cursor::new(buf);
    let set = read_xattr_definitions(&mut cursor).expect("datum below raised cap must decode");
    set_max_alloc(restore);

    assert_eq!(set.len(), 1);
    let entry = &set.entries()[0];
    assert!(entry.is_abbreviated());
    assert_eq!(entry.datum_len(), datum_len as usize);
}

/// With `--max-alloc` raised, a datum one byte past the raised ceiling is still
/// rejected as RERR_PROTOCOL: the bound tracks the effective ceiling, not a
/// fixed constant. Uses a small ceiling so the rejected length stays tiny.
#[test]
fn recv_xattr_values_rejects_above_raised_max_alloc() {
    use crate::max_alloc::{effective_max_alloc, set_max_alloc};

    let restore = effective_max_alloc();
    let raised = 4096usize;
    set_max_alloc(raised);

    let mut list = XattrList::new();
    list.push(XattrEntry::abbreviated(
        b"user.test".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        100,
    ));

    let mut buf = Vec::new();
    write_varint(&mut buf, (raised + 1) as i32).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr_values(&mut cursor, &mut list);
    set_max_alloc(restore);

    let err = result.expect_err("length past raised cap must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds maximum"));
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized xattr datum must stay tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

/// With `--max-alloc` raised below the effective ceiling, a length the old 1 GiB
/// default would have accepted is now rejected - proving the check keys off the
/// effective ceiling rather than the former constant.
#[test]
fn recv_xattr_values_lowered_max_alloc_rejects_sub_gib_length() {
    use crate::max_alloc::{DEFAULT_MAX_ALLOC, effective_max_alloc, set_max_alloc};

    let restore = effective_max_alloc();
    let lowered = 64usize;
    set_max_alloc(lowered);

    let mut list = XattrList::new();
    list.push(XattrEntry::abbreviated(
        b"user.test".to_vec(),
        vec![0u8; MAX_XATTR_DIGEST_LEN],
        100,
    ));

    // 200 bytes: far below the 1 GiB default, but above the lowered ceiling.
    let length = 200usize;
    assert!(length < DEFAULT_MAX_ALLOC);
    assert!(length > lowered);

    let mut buf = Vec::new();
    write_varint(&mut buf, length as i32).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_xattr_values(&mut cursor, &mut list);
    set_max_alloc(restore);

    let err = result.expect_err("length above lowered cap must be rejected");
    assert!(err.to_string().contains("exceeds maximum"));
}

/// A negative datum varint cannot fit the signed-`int32` field encoding, so it
/// stays a protocol violation (exit 2) even when `--max-alloc` is raised to the
/// field maximum. WHY: upstream's read_varint_size rejects a negative value
/// with RERR_PROTOCOL regardless of `max_alloc` (io.c:1917-1926); raising the
/// allocation ceiling must never make a malformed field decode.
#[test]
fn read_definitions_negative_datum_rejected_even_when_max_alloc_raised() {
    use crate::max_alloc::{effective_max_alloc, set_max_alloc};

    let restore = effective_max_alloc();
    set_max_alloc(0x7fff_ffff); // field maximum

    let mut buf = Vec::new();
    write_varint(&mut buf, 1).unwrap(); // count
    write_varint(&mut buf, 10).unwrap(); // name_len
    write_varint(&mut buf, -1).unwrap(); // negative datum_len

    let mut cursor = Cursor::new(buf);
    let result = read_xattr_definitions(&mut cursor);
    set_max_alloc(restore);

    let err = result.expect_err("negative datum_len must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "negative datum must map to RERR_PROTOCOL (2)"
    );
}

/// Pins the literal-xattr wire layout to exact bytes and asserts it is
/// protocol-version independent.
///
/// Upstream `xattrs.c:send_xattr()` has NO `protocol_version` branch: the
/// literal encoding (ndx+1, count, per-entry name_len/datum_len/name+NUL/value)
/// is byte-identical for protocol 30, 31, and 32. The `CF_AVOID_XATTR_OPTIM`
/// capability and `want_xattr_optim` gate (compat.c:747, only active at
/// protocol >= 31) affect the transfer-phase hardlink optimisation, not this
/// flist wire format. This test guards against ever re-introducing a
/// version-conditioned xattr wire encoding, which desynced proto-30 peers with
/// an "xa index out of range" error.
#[test]
fn send_xattr_literal_wire_is_protocol_independent_golden() {
    let mut list = XattrList::new();
    list.push(XattrEntry::new(b"user.foo".to_vec(), b"bar1".to_vec()));
    list.push(XattrEntry::new(b"user.baz".to_vec(), b"qux2".to_vec()));

    let mut buf = Vec::new();
    send_xattr(&mut buf, &list, None, 0).unwrap();

    // ndx+1=0 (literal), count=2, then per entry: name_len (incl NUL),
    // datum_len, name bytes, NUL, value bytes. All varints here are < 0x80 so
    // encode as a single byte, matching upstream write_varint.
    let expected: Vec<u8> = [
        &[0x00u8, 0x02][..], // ndx+1 = 0, count = 2
        &[0x09, 0x04][..],   // name_len = 9, datum_len = 4
        b"user.foo",
        &[0x00][..],
        b"bar1",
        &[0x09, 0x04][..], // name_len = 9, datum_len = 4
        b"user.baz",
        &[0x00][..],
        b"qux2",
    ]
    .concat();

    assert_eq!(
        buf, expected,
        "literal xattr wire bytes drifted from golden"
    );
}
