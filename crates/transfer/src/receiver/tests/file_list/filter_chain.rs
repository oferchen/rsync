//! Receiver-side filter chain: protect rules gate `delete_extraneous_files`
//! and an empty chain is a no-op. Verifies the set/get accessor pair on
//! `ReceiverContext`.

use std::ffi::OsString;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{TestDeletionWriter, test_config, test_handshake};

#[test]
fn receiver_filter_chain_protects_from_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Create files at destination (extra files that should be deleted)
    std::fs::write(dest.join("normal.txt"), b"delete me").unwrap();
    std::fs::write(dest.join("protected.conf"), b"keep me").unwrap();
    std::fs::write(dest.join("source.txt"), b"from sender").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // File list includes "." and "source.txt" - anything else at dest is extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("source.txt".into(), 11, 0o644));

    // Set up filter chain with protect rule for *.conf
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::protect("*.conf")]).unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    // normal.txt should be deleted (not in file list, not protected)
    assert!(
        !dest.join("normal.txt").exists(),
        "normal.txt should be deleted"
    );

    // protected.conf should survive due to protect rule
    assert!(
        dest.join("protected.conf").exists(),
        "protected.conf should be protected from deletion"
    );

    // source.txt should survive (it's in the file list)
    assert!(dest.join("source.txt").exists());

    assert!(stats.files >= 1); // At least normal.txt was deleted
}

#[test]
fn receiver_filter_chain_empty_allows_all_deletions() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("file1.txt"), b"data1").unwrap();
    std::fs::write(dest.join("file2.log"), b"data2").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // File list has "." and "keep.txt" - file1/file2 are extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 4, 0o644));

    // Empty filter chain - all deletions should proceed
    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(!dest.join("file1.txt").exists());
    assert!(!dest.join("file2.log").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(stats.files, 2);
}

/// Regression test for the upstream-testsuite `daemon-delete-stats` failure.
///
/// The test's daemon config carries a global `exclude = ? foobar.baz` rule.
/// Single-character glob `?` previously caused deletion to skip every
/// top-level extraneous file because `delete_extraneous_files()` queried the
/// filter chain with `"./<name>"`. The descendant matcher `?/**` derived
/// from the bare `?` pattern then treated `.` as a single-character parent
/// directory and incorrectly excluded the candidate, leaving
/// `delete.txt` in place. The fix is to strip the implicit `.` prefix
/// before consulting the filter chain at the deletion root.
#[test]
fn single_char_wildcard_exclude_does_not_block_top_level_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("delete.txt"), b"delete\n").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep\n").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Source advertises only `.` and `keep.txt`; `delete.txt` is extraneous.
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 5, 0o644));

    // Daemon-level `exclude = ? foobar.baz` reproduction.
    let global = ::filters::FilterSet::from_rules([
        ::filters::FilterRule::exclude("?"),
        ::filters::FilterRule::exclude("foobar.baz"),
    ])
    .unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(
        !dest.join("delete.txt").exists(),
        "delete.txt must be deleted despite the `?` exclude rule"
    );
    assert!(dest.join("keep.txt").exists(), "keep.txt must survive");
    assert_eq!(stats.files, 1);
}

#[test]
fn receiver_set_and_get_filter_chain() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Default filter chain should be empty
    assert!(ctx.filter_chain().is_empty());

    // Set a chain with rules
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::exclude("*.bak")]).unwrap();
    let chain = ::filters::FilterChain::new(global);
    ctx.set_filter_chain(chain);

    assert!(!ctx.filter_chain().is_empty());
}
