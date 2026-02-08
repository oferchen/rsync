// Extended attribute (xattr) handling tests
//
// These tests cover:
// - Basic xattr preservation
// - Multiple xattrs on same file
// - Large xattr values
// - xattr on directories
// - --xattrs flag behavior
// - xattr filtering
// - xattr synchronization edge cases

#[cfg(all(unix, feature = "xattr"))]
mod xattr_tests {
    use super::*;

    /// Helper to check if xattrs are supported on the current filesystem.
    fn xattrs_supported(path: &Path) -> bool {
        match xattr::set(path, "user.test_support", b"test") {
            Ok(()) => {
                let _ = xattr::remove(path, "user.test_support");
                true
            }
            Err(_) => false,
        }
    }

    // ==================== Basic xattr Preservation Tests ====================

    #[test]
    fn execute_copies_file_with_single_xattr() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"xattr content").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.mime_type", b"text/plain").expect("set xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        let copied = xattr::get(&destination, "user.mime_type")
            .expect("read dest xattr")
            .expect("xattr present");
        assert_eq!(copied, b"text/plain");
    }

    #[test]
    fn execute_without_xattrs_flag_does_not_copy_xattrs() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"no xattr copy").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.test_attr", b"should_not_copy").expect("set xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        assert_eq!(fs::read(&destination).expect("read dest"), b"no xattr copy");

        // Without xattrs flag, no xattrs should be copied
        let dest_xattr = xattr::get(&destination, "user.test_attr").expect("read xattr");
        assert!(dest_xattr.is_none(), "xattr should not be copied without --xattrs");
    }

    // ==================== Multiple xattrs Tests ====================

    #[test]
    fn execute_copies_multiple_xattrs_on_same_file() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"multi xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Set multiple xattrs with different types of values
        xattr::set(&source, "user.attr1", b"value1").expect("set attr1");
        xattr::set(&source, "user.attr2", b"value2").expect("set attr2");
        xattr::set(&source, "user.attr3", b"value3").expect("set attr3");
        xattr::set(&source, "user.metadata.author", b"test_user").expect("set author");
        xattr::set(&source, "user.metadata.version", b"1.0").expect("set version");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        // Verify all xattrs were copied
        assert_eq!(
            xattr::get(&destination, "user.attr1").expect("read").expect("present"),
            b"value1"
        );
        assert_eq!(
            xattr::get(&destination, "user.attr2").expect("read").expect("present"),
            b"value2"
        );
        assert_eq!(
            xattr::get(&destination, "user.attr3").expect("read").expect("present"),
            b"value3"
        );
        assert_eq!(
            xattr::get(&destination, "user.metadata.author").expect("read").expect("present"),
            b"test_user"
        );
        assert_eq!(
            xattr::get(&destination, "user.metadata.version").expect("read").expect("present"),
            b"1.0"
        );
    }

    #[test]
    fn execute_handles_many_xattrs_on_file() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"many xattrs").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Set 20 xattrs
        for i in 0..20 {
            let name = format!("user.attr_{i:02}");
            let value = format!("value_{i:02}");
            xattr::set(&source, &name, value.as_bytes()).expect("set xattr");
        }

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        // Verify all 20 xattrs were copied
        for i in 0..20 {
            let name = format!("user.attr_{i:02}");
            let expected_value = format!("value_{i:02}");
            let copied = xattr::get(&destination, &name)
                .expect("read xattr")
                .expect("xattr present");
            assert_eq!(copied, expected_value.as_bytes());
        }
    }

    // ==================== Large xattr Value Tests ====================

    #[test]
    fn execute_copies_large_xattr_value() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"large xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Create a large xattr value (4KB)
        let large_value: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
        if xattr::set(&source, "user.large_data", &large_value).is_err() {
            eprintln!("filesystem does not support large xattr values, skipping test");
            return;
        }

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        let copied = xattr::get(&destination, "user.large_data")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, large_value);
    }

    #[test]
    fn execute_copies_xattr_with_binary_data() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"binary xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Binary data including null bytes
        let binary_value: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, 0x00, 0xAB, 0xCD, 0x00];
        xattr::set(&source, "user.binary", &binary_value).expect("set binary xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        let copied = xattr::get(&destination, "user.binary")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, binary_value);
    }

    #[test]
    fn execute_copies_empty_xattr_value() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"empty xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Empty xattr value
        xattr::set(&source, "user.empty", b"").expect("set empty xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        let copied = xattr::get(&destination, "user.empty")
            .expect("read xattr")
            .expect("xattr present");
        assert!(copied.is_empty());
    }

    // ==================== xattr on Directories Tests ====================

    #[test]
    fn execute_copies_xattr_on_directory() {
        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("source_dir");
        let dest_dir = temp.path().join("dest_dir");
        fs::create_dir_all(&source_dir).expect("create source dir");

        if !xattrs_supported(&source_dir) {
            eprintln!("xattrs not supported on directories, skipping test");
            return;
        }

        xattr::set(&source_dir, "user.dir_attr", b"directory_value").expect("set dir xattr");

        let operands = vec![
            source_dir.into_os_string(),
            dest_dir.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert!(summary.directories_created() >= 1);

        let copied = xattr::get(&dest_dir, "user.dir_attr")
            .expect("read dir xattr")
            .expect("xattr present");
        assert_eq!(copied, b"directory_value");
    }

    #[test]
    fn execute_copies_xattrs_on_nested_directories() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let nested = source_root.join("level1").join("level2");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(nested.join("file.txt"), b"content").expect("write file");

        if !xattrs_supported(&source_root) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Set xattrs at different directory levels
        xattr::set(&source_root, "user.root_attr", b"root").expect("set root xattr");
        xattr::set(source_root.join("level1"), "user.level1_attr", b"level1").expect("set level1 xattr");
        xattr::set(&nested, "user.level2_attr", b"level2").expect("set level2 xattr");

        let dest_root = temp.path().join("dest");
        let operands = vec![
            source_root.into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        assert!(summary.directories_created() >= 2);

        // Verify xattrs at all levels
        assert_eq!(
            xattr::get(&dest_root, "user.root_attr")
                .expect("read")
                .expect("present"),
            b"root"
        );
        assert_eq!(
            xattr::get(dest_root.join("level1"), "user.level1_attr")
                .expect("read")
                .expect("present"),
            b"level1"
        );
        assert_eq!(
            xattr::get(dest_root.join("level1").join("level2"), "user.level2_attr")
                .expect("read")
                .expect("present"),
            b"level2"
        );
    }

    #[test]
    fn execute_copies_xattrs_on_both_files_and_directories() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source");
        fs::write(source_root.join("file.txt"), b"file content").expect("write file");

        if !xattrs_supported(&source_root) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Set xattr on directory
        xattr::set(&source_root, "user.dir_metadata", b"dir_value").expect("set dir xattr");
        // Set xattr on file
        xattr::set(source_root.join("file.txt"), "user.file_metadata", b"file_value")
            .expect("set file xattr");

        let dest_root = temp.path().join("dest");
        let operands = vec![
            source_root.into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        // Verify directory xattr
        assert_eq!(
            xattr::get(&dest_root, "user.dir_metadata")
                .expect("read")
                .expect("present"),
            b"dir_value"
        );
        // Verify file xattr
        assert_eq!(
            xattr::get(dest_root.join("file.txt"), "user.file_metadata")
                .expect("read")
                .expect("present"),
            b"file_value"
        );
    }

    // ==================== xattr Update/Sync Tests ====================

    #[test]
    fn execute_updates_existing_xattr_value() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"update xattr").expect("write source");
        fs::write(&destination, b"update xattr").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Source has updated value
        xattr::set(&source, "user.version", b"2.0").expect("set source xattr");
        // Destination has old value
        xattr::set(&destination, "user.version", b"1.0").expect("set dest xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let _summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true).ignore_times(true),
            )
            .expect("copy succeeds");

        // Verify xattr was updated
        let copied = xattr::get(&destination, "user.version")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, b"2.0");
    }

    #[test]
    fn execute_removes_extra_xattrs_from_destination() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"remove xattr").expect("write source");
        fs::write(&destination, b"remove xattr").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Source has one xattr
        xattr::set(&source, "user.keep", b"keep_value").expect("set source xattr");
        // Destination has extra xattr
        xattr::set(&destination, "user.keep", b"old_keep").expect("set dest keep");
        xattr::set(&destination, "user.extra", b"extra_value").expect("set dest extra");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let _summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true).ignore_times(true),
            )
            .expect("copy succeeds");

        // Verify keep xattr was synced
        assert_eq!(
            xattr::get(&destination, "user.keep")
                .expect("read")
                .expect("present"),
            b"keep_value"
        );
        // Verify extra xattr was removed
        assert!(
            xattr::get(&destination, "user.extra")
                .expect("read")
                .is_none(),
            "extra xattr should be removed"
        );
    }

    #[test]
    fn execute_adds_new_xattrs_to_destination() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"add xattr").expect("write source");
        fs::write(&destination, b"add xattr").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Source has multiple xattrs
        xattr::set(&source, "user.existing", b"existing").expect("set existing");
        xattr::set(&source, "user.new_attr", b"new_value").expect("set new");
        // Destination has only one xattr
        xattr::set(&destination, "user.existing", b"existing").expect("set dest existing");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let _summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true).ignore_times(true),
            )
            .expect("copy succeeds");

        // Verify both xattrs are present
        assert_eq!(
            xattr::get(&destination, "user.existing")
                .expect("read")
                .expect("present"),
            b"existing"
        );
        assert_eq!(
            xattr::get(&destination, "user.new_attr")
                .expect("read")
                .expect("present"),
            b"new_value"
        );
    }

    // ==================== xattr Filter Tests ====================

    #[test]
    fn execute_xattr_filter_excludes_specific_attrs() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"filter xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.keep", b"keep_value").expect("set keep");
        xattr::set(&source, "user.skip", b"skip_value").expect("set skip");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        // Filter that excludes "user.skip" and includes "user.keep"
        // Last matching rule wins, so exclude must come after include
        let program = FilterProgram::new([
            FilterProgramEntry::Rule(FilterRule::exclude("user.skip").with_xattr_only(true)),
            FilterProgramEntry::Rule(FilterRule::include("user.keep").with_xattr_only(true)),
        ])
        .expect("compile program");

        let options = LocalCopyOptions::default()
            .xattrs(true)
            .with_filter_program(Some(program));

        let _summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        // Verify included attr was copied
        assert_eq!(
            xattr::get(&destination, "user.keep")
                .expect("read")
                .expect("present"),
            b"keep_value"
        );
        // Verify excluded attr is absent
        assert!(
            xattr::get(&destination, "user.skip")
                .expect("read")
                .is_none(),
            "excluded xattr should not be copied"
        );
    }

    #[test]
    fn execute_xattr_filter_includes_only_matching_patterns() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"include only").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.keep_one", b"one").expect("set one");
        xattr::set(&source, "user.keep_two", b"two").expect("set two");
        xattr::set(&source, "user.skip_this", b"skip").expect("set skip");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        // Filter: exclude all, then include specific ones
        // Last matching rule wins, so we list them in order
        let program = FilterProgram::new([
            FilterProgramEntry::Rule(FilterRule::exclude("user.skip_this").with_xattr_only(true)),
            FilterProgramEntry::Rule(FilterRule::include("user.keep_one").with_xattr_only(true)),
            FilterProgramEntry::Rule(FilterRule::include("user.keep_two").with_xattr_only(true)),
        ])
        .expect("compile program");

        let options = LocalCopyOptions::default()
            .xattrs(true)
            .with_filter_program(Some(program));

        let _summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        // Verify included attrs were copied
        assert_eq!(
            xattr::get(&destination, "user.keep_one")
                .expect("read")
                .expect("present"),
            b"one"
        );
        assert_eq!(
            xattr::get(&destination, "user.keep_two")
                .expect("read")
                .expect("present"),
            b"two"
        );
        // Verify excluded attr is absent
        assert!(
            xattr::get(&destination, "user.skip_this")
                .expect("read")
                .is_none(),
            "excluded xattr should not be copied"
        );
    }

    // ==================== Dry Run Tests ====================

    #[test]
    fn execute_dry_run_does_not_copy_xattrs() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"dry run xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.dry_run", b"should_not_copy").expect("set xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::DryRun,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("dry run succeeds");

        assert_eq!(summary.files_copied(), 1);
        // Destination should not exist
        assert!(!destination.exists(), "dry run should not create file");
    }

    // ==================== Edge Cases ====================

    #[test]
    fn execute_handles_special_characters_in_xattr_name() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"special name").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // xattr names with various special characters (that are valid)
        xattr::set(&source, "user.with.dots", b"dots").expect("set dots");
        xattr::set(&source, "user.with_underscores", b"underscores").expect("set underscores");
        xattr::set(&source, "user.with-dashes", b"dashes").expect("set dashes");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let _summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        // Verify all special-named xattrs were copied
        assert_eq!(
            xattr::get(&destination, "user.with.dots")
                .expect("read")
                .expect("present"),
            b"dots"
        );
        assert_eq!(
            xattr::get(&destination, "user.with_underscores")
                .expect("read")
                .expect("present"),
            b"underscores"
        );
        assert_eq!(
            xattr::get(&destination, "user.with-dashes")
                .expect("read")
                .expect("present"),
            b"dashes"
        );
    }

    #[test]
    fn execute_handles_utf8_xattr_values() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"utf8 xattr").expect("write source");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // UTF-8 content in xattr value
        let utf8_value = "Hello, 世界! 🌍 Привет!";
        xattr::set(&source, "user.utf8_value", utf8_value.as_bytes()).expect("set utf8 xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let _summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        let copied = xattr::get(&destination, "user.utf8_value")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, utf8_value.as_bytes());
    }

    #[test]
    fn execute_preserves_xattrs_on_file_update() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"updated content").expect("write source");
        fs::write(&destination, b"old content").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(&source, "user.preserved", b"preserved_value").expect("set xattr");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);

        // Verify file content was updated
        assert_eq!(fs::read(&destination).expect("read dest"), b"updated content");
        // Verify xattr was preserved
        let copied = xattr::get(&destination, "user.preserved")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, b"preserved_value");
    }

    #[test]
    fn execute_copies_multiple_files_with_different_xattrs() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source dir");

        fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
        fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");
        fs::write(source_root.join("file3.txt"), b"content3").expect("write file3");

        if !xattrs_supported(&source_root.join("file1.txt")) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        xattr::set(source_root.join("file1.txt"), "user.file1_attr", b"value1")
            .expect("set file1 xattr");
        xattr::set(source_root.join("file2.txt"), "user.file2_attr", b"value2")
            .expect("set file2 xattr");
        xattr::set(source_root.join("file3.txt"), "user.file3_attr", b"value3")
            .expect("set file3 xattr");

        let dest_root = temp.path().join("dest");
        let mut source_operand = source_root.into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());

        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 3);

        // Verify each file has its own xattr
        assert_eq!(
            xattr::get(dest_root.join("file1.txt"), "user.file1_attr")
                .expect("read")
                .expect("present"),
            b"value1"
        );
        assert_eq!(
            xattr::get(dest_root.join("file2.txt"), "user.file2_attr")
                .expect("read")
                .expect("present"),
            b"value2"
        );
        assert_eq!(
            xattr::get(dest_root.join("file3.txt"), "user.file3_attr")
                .expect("read")
                .expect("present"),
            b"value3"
        );
    }

    // ==================== Symlink xattr Tests ====================

    #[cfg(unix)]
    #[test]
    fn execute_preserves_xattrs_with_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let source_file = temp.path().join("real_file.txt");
        let source_link = temp.path().join("link.txt");
        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest");

        fs::write(&source_file, b"real content").expect("write file");
        symlink(&source_file, &source_link).expect("create symlink");

        if !xattrs_supported(&source_file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Set xattr on the real file
        xattr::set(&source_file, "user.real_file_attr", b"real_value").expect("set xattr");

        let operands = vec![
            source_file.into_os_string(),
            source_link.into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default()
            .xattrs(true)
            .links(true);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(summary.files_copied() >= 1);

        // Verify xattr on the copied real file
        let copied = xattr::get(dest_root.join("real_file.txt"), "user.real_file_attr")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, b"real_value");
    }
}
