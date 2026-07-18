//! Tests for large-scale scenarios.

use std::path::PathBuf;

use crate::local_copy::hard_links::HardLinkTracker;
use crate::local_copy::test_support;

/// Test tracking many hardlink groups.
#[test]
fn many_hardlink_groups() {
    let temp = test_support::create_tempdir();
    let num_groups = 50;
    let mut tracker = HardLinkTracker::new();

    for group in 0..num_groups {
        let file1 = temp.path().join(format!("group{group}_a.txt"));
        let file2 = temp.path().join(format!("group{group}_b.txt"));

        std::fs::write(&file1, format!("group{group} content")).unwrap();
        std::fs::hard_link(&file1, &file2).unwrap();

        let meta1 = std::fs::metadata(&file1).unwrap();
        let dest = PathBuf::from(format!("/dest/group{group}_a.txt"));
        tracker.record(&meta1, &dest);

        let meta2 = std::fs::metadata(&file2).unwrap();
        let existing = tracker.existing_target(&meta2);
        assert_eq!(existing, Some(dest.clone()));
    }
}

/// Test many links to the same file.
#[test]
fn many_links_to_same_file() {
    let temp = test_support::create_tempdir();
    let original = temp.path().join("original.txt");
    std::fs::write(&original, "content").unwrap();

    let num_links = 100;
    let links: Vec<_> = (0..num_links)
        .map(|i| {
            let link = temp.path().join(format!("link{i}.txt"));
            std::fs::hard_link(&original, &link).unwrap();
            link
        })
        .collect();

    let mut tracker = HardLinkTracker::new();
    let original_meta = std::fs::metadata(&original).unwrap();
    let dest = PathBuf::from("/dest/original.txt");
    tracker.record(&original_meta, &dest);

    for (i, link) in links.iter().enumerate() {
        let meta = std::fs::metadata(link).unwrap();
        let existing = tracker.existing_target(&meta);
        assert_eq!(
            existing,
            Some(dest.clone()),
            "link {i} should resolve to original destination"
        );
    }
}
