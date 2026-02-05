//! Comprehensive tests for permission preservation.
//!
//! This module contains extensive test coverage for permission preservation
//! functionality, including:
//! 1. Basic permissions (rwx) preservation for files and directories
//! 2. Special bits (setuid, setgid, sticky bit) handling
//! 3. Umask independence verification
//! 4. Round-trip preservation of all permission bits
//! 5. Edge cases and error conditions

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use crate::{
        apply_directory_metadata, apply_directory_metadata_with_options, apply_file_metadata,
        apply_file_metadata_with_options, MetadataOptions,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::tempdir;

    /// Helper to get the raw mode bits from a file, including special bits.
    fn get_mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
    }

    /// Helper to set mode bits on a file.
    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, PermissionsExt::from_mode(mode)).expect("set permissions");
    }

    // ========================================================================
    // Section 1: Basic Permission Tests (rwx)
    // ========================================================================

    #[test]
    fn file_rwx_permissions_preserved_644() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_644.txt");
        let dest = temp.path().join("dest_644.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o644);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o644);
    }

    #[test]
    fn file_rwx_permissions_preserved_755() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_755.txt");
        let dest = temp.path().join("dest_755.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o755);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o755);
    }

    #[test]
    fn file_rwx_permissions_preserved_600() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_600.txt");
        let dest = temp.path().join("dest_600.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o600);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o600);
    }

    #[test]
    fn file_rwx_permissions_preserved_777() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_777.txt");
        let dest = temp.path().join("dest_777.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o777);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o777);
    }

    #[test]
    fn file_rwx_permissions_preserved_000() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_000.txt");
        let dest = temp.path().join("dest_000.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o000);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Verify we can read the mode even on a 000 file
        let mode = get_mode(&dest) & 0o777;
        assert_eq!(mode, 0o000);
    }

    #[test]
    fn directory_rwx_permissions_preserved_755() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_dir_755");
        let dest = temp.path().join("dest_dir_755");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        set_mode(&source, 0o755);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o755);
    }

    #[test]
    fn directory_rwx_permissions_preserved_700() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_dir_700");
        let dest = temp.path().join("dest_dir_700");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        set_mode(&source, 0o700);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o700);
    }

    #[test]
    fn directory_rwx_permissions_preserved_750() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_dir_750");
        let dest = temp.path().join("dest_dir_750");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        set_mode(&source, 0o750);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o750);
    }

    #[test]
    fn all_basic_permission_combinations() {
        let temp = tempdir().expect("tempdir");

        // Test a representative sample of permission combinations
        let test_modes = [
            0o400, 0o200, 0o100, // Individual bits
            0o640, 0o660, 0o664, // Common patterns
            0o444, 0o555, 0o666, // All same
            0o123, 0o321, 0o246, // Varied patterns
        ];

        for (i, &mode) in test_modes.iter().enumerate() {
            let source = temp.path().join(format!("source_mode_{}.txt", i));
            let dest = temp.path().join(format!("dest_mode_{}.txt", i));

            fs::write(&source, b"test").expect("write source");
            fs::write(&dest, b"test").expect("write dest");

            set_mode(&source, mode);
            let metadata = fs::metadata(&source).expect("metadata");

            apply_file_metadata(&dest, &metadata).expect("apply metadata");

            assert_eq!(
                get_mode(&dest) & 0o777,
                mode,
                "Failed for mode {:o}",
                mode
            );
        }
    }

    // ========================================================================
    // Section 2: Special Bits (setuid, setgid, sticky)
    // ========================================================================

    #[test]
    fn setuid_bit_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_setuid.txt");
        let dest = temp.path().join("dest_setuid.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set mode with setuid bit (04755)
        set_mode(&source, 0o4755);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Verify setuid bit is preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o4755);
        assert_eq!(get_mode(&dest) & 0o4000, 0o4000, "setuid bit not set");
    }

    #[test]
    fn setgid_bit_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_setgid.txt");
        let dest = temp.path().join("dest_setgid.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set mode with setgid bit (02755)
        set_mode(&source, 0o2755);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Verify setgid bit is preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o2755);
        assert_eq!(get_mode(&dest) & 0o2000, 0o2000, "setgid bit not set");
    }

    #[test]
    fn sticky_bit_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_sticky");
        let dest = temp.path().join("dest_sticky");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        // Set mode with sticky bit (01777)
        set_mode(&source, 0o1777);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata(&dest, &metadata).expect("apply metadata");

        // Verify sticky bit is preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o1777);
        assert_eq!(get_mode(&dest) & 0o1000, 0o1000, "sticky bit not set");
    }

    #[test]
    fn setuid_and_setgid_both_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_setuid_setgid.txt");
        let dest = temp.path().join("dest_setuid_setgid.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set mode with both setuid and setgid (06755)
        set_mode(&source, 0o6755);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Verify both bits are preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o6755);
        assert_eq!(get_mode(&dest) & 0o4000, 0o4000, "setuid bit not set");
        assert_eq!(get_mode(&dest) & 0o2000, 0o2000, "setgid bit not set");
    }

    #[test]
    fn all_special_bits_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_all_special.txt");
        let dest = temp.path().join("dest_all_special.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set mode with all special bits (07777)
        set_mode(&source, 0o7777);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Verify all special bits are preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o7777);
        assert_eq!(get_mode(&dest) & 0o4000, 0o4000, "setuid bit not set");
        assert_eq!(get_mode(&dest) & 0o2000, 0o2000, "setgid bit not set");
        assert_eq!(get_mode(&dest) & 0o1000, 0o1000, "sticky bit not set");
    }

    #[test]
    fn setuid_with_minimal_permissions() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_setuid_min.txt");
        let dest = temp.path().join("dest_setuid_min.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Setuid with minimal permissions (04000)
        set_mode(&source, 0o4000);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o7777, 0o4000);
    }

    #[test]
    fn setgid_on_directory() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_setgid_dir");
        let dest = temp.path().join("dest_setgid_dir");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        // Setgid on directory (common for shared directories)
        set_mode(&source, 0o2775);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o7777, 0o2775);
        assert_eq!(get_mode(&dest) & 0o2000, 0o2000, "setgid bit not set");
    }

    #[test]
    fn special_bits_cleared_when_source_has_none() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_no_special.txt");
        let dest = temp.path().join("dest_with_special.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Source has no special bits
        set_mode(&source, 0o644);
        // Dest has all special bits
        set_mode(&dest, 0o7777);

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Special bits should be cleared
        assert_eq!(get_mode(&dest) & 0o7777, 0o644);
        assert_eq!(get_mode(&dest) & 0o7000, 0o0000, "special bits not cleared");
    }

    // ========================================================================
    // Section 3: Umask Independence
    // ========================================================================

    #[test]
    fn permissions_preserved_regardless_of_umask() {
        use libc::{mode_t, umask};

        let temp = tempdir().expect("tempdir");

        // Test with different umask values
        let umasks = [0o022, 0o077, 0o002, 0o000, 0o027];
        let test_mode = 0o755;

        for (i, &test_umask) in umasks.iter().enumerate() {
            let source = temp.path().join(format!("source_umask_{}.txt", i));
            let dest = temp.path().join(format!("dest_umask_{}.txt", i));

            fs::write(&source, b"test").expect("write source");
            fs::write(&dest, b"test").expect("write dest");

            set_mode(&source, test_mode);
            let metadata = fs::metadata(&source).expect("metadata");

            // Change umask
            let old_umask = unsafe { umask(test_umask as mode_t) };

            apply_file_metadata(&dest, &metadata).expect("apply metadata");

            // Restore umask
            unsafe { umask(old_umask) };

            // Verify permissions are preserved exactly, regardless of umask
            assert_eq!(
                get_mode(&dest) & 0o777,
                test_mode,
                "Failed with umask {:o}",
                test_umask
            );
        }
    }

    #[test]
    fn restrictive_umask_does_not_interfere() {
        use libc::{mode_t, umask};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_restrictive.txt");
        let dest = temp.path().join("dest_restrictive.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set permissive mode on source
        set_mode(&source, 0o777);
        let metadata = fs::metadata(&source).expect("metadata");

        // Apply with very restrictive umask
        let old_umask = unsafe { umask(0o077 as mode_t) };
        apply_file_metadata(&dest, &metadata).expect("apply metadata");
        unsafe { umask(old_umask) };

        // Permissions should still be 0o777, not restricted by umask
        assert_eq!(get_mode(&dest) & 0o777, 0o777);
    }

    #[test]
    fn permissive_umask_does_not_add_permissions() {
        use libc::{mode_t, umask};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_permissive.txt");
        let dest = temp.path().join("dest_permissive.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set restrictive mode on source
        set_mode(&source, 0o600);
        let metadata = fs::metadata(&source).expect("metadata");

        // Apply with permissive umask
        let old_umask = unsafe { umask(0o000 as mode_t) };
        apply_file_metadata(&dest, &metadata).expect("apply metadata");
        unsafe { umask(old_umask) };

        // Permissions should still be 0o600, not expanded by umask
        assert_eq!(get_mode(&dest) & 0o777, 0o600);
    }

    #[test]
    fn special_bits_preserved_with_restrictive_umask() {
        use libc::{mode_t, umask};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_special_umask.txt");
        let dest = temp.path().join("dest_special_umask.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o6755);
        let metadata = fs::metadata(&source).expect("metadata");

        // Apply with restrictive umask
        let old_umask = unsafe { umask(0o077 as mode_t) };
        apply_file_metadata(&dest, &metadata).expect("apply metadata");
        unsafe { umask(old_umask) };

        // Special bits should be preserved
        assert_eq!(get_mode(&dest) & 0o7777, 0o6755);
    }

    // ========================================================================
    // Section 4: Round-trip Preservation
    // ========================================================================

    #[test]
    fn round_trip_preserves_basic_permissions() {
        let temp = tempdir().expect("tempdir");
        let file1 = temp.path().join("file1.txt");
        let file2 = temp.path().join("file2.txt");
        let file3 = temp.path().join("file3.txt");

        fs::write(&file1, b"test").expect("write file1");
        fs::write(&file2, b"test").expect("write file2");
        fs::write(&file3, b"test").expect("write file3");

        // Set initial mode
        let original_mode = 0o642;
        set_mode(&file1, original_mode);

        // First transfer
        let meta1 = fs::metadata(&file1).expect("metadata 1");
        apply_file_metadata(&file2, &meta1).expect("apply 1");

        // Second transfer
        let meta2 = fs::metadata(&file2).expect("metadata 2");
        apply_file_metadata(&file3, &meta2).expect("apply 2");

        // Verify mode is preserved through both transfers
        assert_eq!(get_mode(&file1) & 0o777, original_mode);
        assert_eq!(get_mode(&file2) & 0o777, original_mode);
        assert_eq!(get_mode(&file3) & 0o777, original_mode);
    }

    #[test]
    fn round_trip_preserves_special_bits() {
        let temp = tempdir().expect("tempdir");
        let file1 = temp.path().join("special1.txt");
        let file2 = temp.path().join("special2.txt");
        let file3 = temp.path().join("special3.txt");

        fs::write(&file1, b"test").expect("write file1");
        fs::write(&file2, b"test").expect("write file2");
        fs::write(&file3, b"test").expect("write file3");

        // Set mode with all special bits
        let original_mode = 0o7755;
        set_mode(&file1, original_mode);

        // First transfer
        let meta1 = fs::metadata(&file1).expect("metadata 1");
        apply_file_metadata(&file2, &meta1).expect("apply 1");

        // Second transfer
        let meta2 = fs::metadata(&file2).expect("metadata 2");
        apply_file_metadata(&file3, &meta2).expect("apply 2");

        // Verify all bits preserved through both transfers
        assert_eq!(get_mode(&file1) & 0o7777, original_mode);
        assert_eq!(get_mode(&file2) & 0o7777, original_mode);
        assert_eq!(get_mode(&file3) & 0o7777, original_mode);
    }

    #[test]
    fn round_trip_preserves_all_permission_bits() {
        let temp = tempdir().expect("tempdir");

        // Test multiple permission patterns through round-trip
        let test_modes = [
            0o000, 0o644, 0o755, 0o777, 0o4755, 0o2755, 0o1777, 0o6755, 0o7777,
        ];

        for (i, &mode) in test_modes.iter().enumerate() {
            let file1 = temp.path().join(format!("rt1_{}.txt", i));
            let file2 = temp.path().join(format!("rt2_{}.txt", i));
            let file3 = temp.path().join(format!("rt3_{}.txt", i));
            let file4 = temp.path().join(format!("rt4_{}.txt", i));

            fs::write(&file1, b"test").expect("write file1");
            fs::write(&file2, b"test").expect("write file2");
            fs::write(&file3, b"test").expect("write file3");
            fs::write(&file4, b"test").expect("write file4");

            set_mode(&file1, mode);

            // Triple round-trip
            let m1 = fs::metadata(&file1).expect("m1");
            apply_file_metadata(&file2, &m1).expect("a1");

            let m2 = fs::metadata(&file2).expect("m2");
            apply_file_metadata(&file3, &m2).expect("a2");

            let m3 = fs::metadata(&file3).expect("m3");
            apply_file_metadata(&file4, &m3).expect("a3");

            // All should have the same mode
            assert_eq!(get_mode(&file1) & 0o7777, mode, "file1 mode mismatch");
            assert_eq!(get_mode(&file2) & 0o7777, mode, "file2 mode mismatch");
            assert_eq!(get_mode(&file3) & 0o7777, mode, "file3 mode mismatch");
            assert_eq!(get_mode(&file4) & 0o7777, mode, "file4 mode mismatch");
        }
    }

    #[test]
    fn round_trip_with_options_enabled() {
        let temp = tempdir().expect("tempdir");
        let file1 = temp.path().join("opt1.txt");
        let file2 = temp.path().join("opt2.txt");

        fs::write(&file1, b"test").expect("write file1");
        fs::write(&file2, b"test").expect("write file2");

        let mode = 0o4755;
        set_mode(&file1, mode);

        let meta = fs::metadata(&file1).expect("metadata");
        let options = MetadataOptions::new().preserve_permissions(true);

        apply_file_metadata_with_options(&file2, &meta, &options).expect("apply");

        assert_eq!(get_mode(&file2) & 0o7777, mode);
    }

    #[test]
    fn round_trip_directories() {
        let temp = tempdir().expect("tempdir");
        let dir1 = temp.path().join("dir1");
        let dir2 = temp.path().join("dir2");
        let dir3 = temp.path().join("dir3");

        fs::create_dir(&dir1).expect("create dir1");
        fs::create_dir(&dir2).expect("create dir2");
        fs::create_dir(&dir3).expect("create dir3");

        let mode = 0o2775;
        set_mode(&dir1, mode);

        let m1 = fs::metadata(&dir1).expect("m1");
        apply_directory_metadata(&dir2, &m1).expect("a1");

        let m2 = fs::metadata(&dir2).expect("m2");
        apply_directory_metadata(&dir3, &m2).expect("a2");

        assert_eq!(get_mode(&dir1) & 0o7777, mode);
        assert_eq!(get_mode(&dir2) & 0o7777, mode);
        assert_eq!(get_mode(&dir3) & 0o7777, mode);
    }

    // ========================================================================
    // Section 5: Edge Cases and Boundary Conditions
    // ========================================================================

    #[test]
    fn permissions_preserved_when_dest_already_correct() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_same.txt");
        let dest = temp.path().join("dest_same.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        let mode = 0o644;
        set_mode(&source, mode);
        set_mode(&dest, mode);

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        // Should still be correct
        assert_eq!(get_mode(&dest) & 0o777, mode);
    }

    #[test]
    fn permissions_changed_when_dest_different() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_diff.txt");
        let dest = temp.path().join("dest_diff.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o755);
        set_mode(&dest, 0o644);

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o755);
    }

    #[test]
    fn all_permission_bits_masked_correctly() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_mask.txt");
        let dest = temp.path().join("dest_mask.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Set all relevant permission bits
        set_mode(&source, 0o7777);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        let mode = get_mode(&dest);

        // Check individual bit groups
        assert_eq!(mode & 0o400, 0o400, "user read");
        assert_eq!(mode & 0o200, 0o200, "user write");
        assert_eq!(mode & 0o100, 0o100, "user execute");
        assert_eq!(mode & 0o040, 0o040, "group read");
        assert_eq!(mode & 0o020, 0o020, "group write");
        assert_eq!(mode & 0o010, 0o010, "group execute");
        assert_eq!(mode & 0o004, 0o004, "other read");
        assert_eq!(mode & 0o002, 0o002, "other write");
        assert_eq!(mode & 0o001, 0o001, "other execute");
        assert_eq!(mode & 0o4000, 0o4000, "setuid");
        assert_eq!(mode & 0o2000, 0o2000, "setgid");
        assert_eq!(mode & 0o1000, 0o1000, "sticky");
    }

    #[test]
    fn permissions_not_preserved_when_disabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_disabled.txt");
        let dest = temp.path().join("dest_disabled.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o755);
        let original_dest_mode = 0o644;
        set_mode(&dest, original_dest_mode);

        let metadata = fs::metadata(&source).expect("metadata");
        let options = MetadataOptions::new().preserve_permissions(false);

        apply_file_metadata_with_options(&dest, &metadata, &options).expect("apply metadata");

        // Permissions should not change
        assert_eq!(get_mode(&dest) & 0o777, original_dest_mode);
    }

    #[test]
    fn directory_permissions_not_preserved_when_disabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_dir_disabled");
        let dest = temp.path().join("dest_dir_disabled");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        set_mode(&source, 0o755);
        let original_dest_mode = 0o700;
        set_mode(&dest, original_dest_mode);

        let metadata = fs::metadata(&source).expect("metadata");
        let options = MetadataOptions::new().preserve_permissions(false);

        apply_directory_metadata_with_options(&dest, &metadata, options)
            .expect("apply metadata");

        // Permissions should not change
        assert_eq!(get_mode(&dest) & 0o777, original_dest_mode);
    }

    #[test]
    fn zero_permissions_can_be_set_and_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_zero.txt");
        let dest = temp.path().join("dest_zero.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o000);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o000);
    }

    #[test]
    fn special_bits_only_no_rwx() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_special_only.txt");
        let dest = temp.path().join("dest_special_only.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        // Only special bits, no rwx permissions
        set_mode(&source, 0o7000);
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o7777, 0o7000);
    }

    #[test]
    fn odd_permission_combinations() {
        let temp = tempdir().expect("tempdir");

        // Test unusual but valid permission combinations
        let odd_modes = [
            0o1234, // sticky + varied perms
            0o5432, // setuid + sticky + varied
            0o3210, // setgid + sticky + varied
            0o7001, // all special + minimal perms
        ];

        for (i, &mode) in odd_modes.iter().enumerate() {
            let source = temp.path().join(format!("odd_source_{}.txt", i));
            let dest = temp.path().join(format!("odd_dest_{}.txt", i));

            fs::write(&source, b"test").expect("write source");
            fs::write(&dest, b"test").expect("write dest");

            set_mode(&source, mode);
            let metadata = fs::metadata(&source).expect("metadata");

            apply_file_metadata(&dest, &metadata).expect("apply metadata");

            assert_eq!(
                get_mode(&dest) & 0o7777,
                mode,
                "Failed for odd mode {:o}",
                mode
            );
        }
    }

    // ========================================================================
    // Section 6: Integration with MetadataOptions
    // ========================================================================

    #[test]
    fn permissions_with_options_preserve_times_disabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_no_time.txt");
        let dest = temp.path().join("dest_no_time.txt");

        fs::write(&source, b"test").expect("write source");
        fs::write(&dest, b"test").expect("write dest");

        set_mode(&source, 0o755);
        let metadata = fs::metadata(&source).expect("metadata");

        let options = MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(false);

        apply_file_metadata_with_options(&dest, &metadata, &options).expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o777, 0o755);
    }

    #[test]
    fn directory_permissions_with_all_options() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source_all_opts");
        let dest = temp.path().join("dest_all_opts");

        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        set_mode(&source, 0o2755);
        let metadata = fs::metadata(&source).expect("metadata");

        let options = MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(true);

        apply_directory_metadata_with_options(&dest, &metadata, options)
            .expect("apply metadata");

        assert_eq!(get_mode(&dest) & 0o7777, 0o2755);
    }
}
