//! End-to-end AppleDouble container round-trip tests.
//!
//! Cross-platform: the parser/encoder is pure-data and runs identically
//! everywhere. The macOS-only test additionally writes the synthesised
//! container back as `com.apple.ResourceFork` / `com.apple.FinderInfo`
//! xattrs and reads them through the safe accessors to confirm the two
//! halves of the pipeline (sidecar bytes vs native xattrs) carry the same
//! payload.

use apple_fs::apple_double::{AppleDouble, EntryId};

fn finder_info_payload() -> Vec<u8> {
    let mut info = vec![0u8; 32];
    info[0..4].copy_from_slice(b"TEXT");
    info[4..8].copy_from_slice(b"ttxt");
    info
}

fn resource_fork_payload() -> Vec<u8> {
    // Deterministic 4 KiB payload mimicking a small resource map. The exact
    // bytes are not interpreted by the container; what matters is fidelity.
    (0..4096).map(|i| (i as u8).wrapping_mul(31)).collect()
}

#[test]
fn round_trip_finder_info_and_resource_fork_through_apple_double() {
    let mut container = AppleDouble::new();
    container.set_entry(EntryId::FinderInfo, finder_info_payload());
    container.set_entry(EntryId::ResourceFork, resource_fork_payload());

    let bytes = container.encode().expect("encode");
    let decoded = AppleDouble::decode(&bytes).expect("decode");

    assert_eq!(
        decoded.finder_info().expect("finder info present"),
        &finder_info_payload()[..],
    );
    assert_eq!(
        decoded.resource_fork().expect("resource fork present"),
        &resource_fork_payload()[..],
    );
}

#[test]
fn empty_container_round_trips() {
    let container = AppleDouble::new();
    let bytes = container.encode().expect("encode");
    let decoded = AppleDouble::decode(&bytes).expect("decode");
    assert!(decoded.entries.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_resource_fork_pipeline_matches_apple_double_payload() {
    use apple_fs::{read_finder_info, read_resource_fork, write_finder_info, write_resource_fork};
    use std::io;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("subject.txt");
    std::fs::write(&path, b"data fork").expect("write data");

    // Probe filesystem support: skip when xattrs are unavailable (e.g. when
    // /tmp is mounted on a FAT volume).
    if let Err(error) = xattr::set(&path, "com.apple.oc-rsync.probe", b"") {
        if matches!(
            error.kind(),
            io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
        ) {
            eprintln!("skipping: filesystem does not support xattrs");
            return;
        }
    }
    let _ = xattr::remove(&path, "com.apple.oc-rsync.probe");

    let mut info = [0u8; 32];
    info[0..4].copy_from_slice(b"TEXT");
    info[4..8].copy_from_slice(b"ttxt");

    let fork = resource_fork_payload();
    write_finder_info(&path, &info).expect("write finder info");
    write_resource_fork(&path, &fork).expect("write resource fork");

    let observed_info = read_finder_info(&path)
        .expect("read info")
        .expect("present");
    assert_eq!(observed_info, info);
    let observed_fork = read_resource_fork(&path)
        .expect("read fork")
        .expect("present");
    assert_eq!(observed_fork, fork);

    // Build the equivalent AppleDouble container and confirm the same bytes
    // round-trip through the on-disk container format.
    let mut container = AppleDouble::new();
    container.set_entry(EntryId::FinderInfo, info.to_vec());
    container.set_entry(EntryId::ResourceFork, fork);

    let encoded = container.encode().expect("encode container");
    let decoded = AppleDouble::decode(&encoded).expect("decode container");
    assert_eq!(decoded.finder_info().unwrap(), &observed_info[..]);
    assert_eq!(decoded.resource_fork().unwrap(), observed_fork.as_slice());
}
