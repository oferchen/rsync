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

        let dest_xattr = xattr::get(&destination, "user.test_attr").expect("read xattr");
        assert!(
            dest_xattr.is_none(),
            "xattr should not be copied without --xattrs"
        );
    }

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

        assert_eq!(
            xattr::get(&destination, "user.attr1")
                .expect("read")
                .expect("present"),
            b"value1"
        );
        assert_eq!(
            xattr::get(&destination, "user.attr2")
                .expect("read")
                .expect("present"),
            b"value2"
        );
        assert_eq!(
            xattr::get(&destination, "user.attr3")
                .expect("read")
                .expect("present"),
            b"value3"
        );
        assert_eq!(
            xattr::get(&destination, "user.metadata.author")
                .expect("read")
                .expect("present"),
            b"test_user"
        );
        assert_eq!(
            xattr::get(&destination, "user.metadata.version")
                .expect("read")
                .expect("present"),
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

        for i in 0..20 {
            let name = format!("user.attr_{i:02}");
            let expected_value = format!("value_{i:02}");
            let copied = xattr::get(&destination, &name)
                .expect("read xattr")
                .expect("xattr present");
            assert_eq!(copied, expected_value.as_bytes());
        }
    }

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

        xattr::set(&source_root, "user.root_attr", b"root").expect("set root xattr");
        xattr::set(source_root.join("level1"), "user.level1_attr", b"level1")
            .expect("set level1 xattr");
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

        assert_eq!(
            xattr::get(dest_root.join("source"), "user.root_attr")
                .expect("read")
                .expect("present"),
            b"root"
        );
        assert_eq!(
            xattr::get(dest_root.join("source").join("level1"), "user.level1_attr")
                .expect("read")
                .expect("present"),
            b"level1"
        );
        assert_eq!(
            xattr::get(
                dest_root.join("source").join("level1").join("level2"),
                "user.level2_attr"
            )
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

        xattr::set(&source_root, "user.dir_metadata", b"dir_value").expect("set dir xattr");
        xattr::set(
            source_root.join("file.txt"),
            "user.file_metadata",
            b"file_value",
        )
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

        assert_eq!(
            xattr::get(dest_root.join("source"), "user.dir_metadata")
                .expect("read")
                .expect("present"),
            b"dir_value"
        );
        assert_eq!(
            xattr::get(
                dest_root.join("source").join("file.txt"),
                "user.file_metadata"
            )
            .expect("read")
            .expect("present"),
            b"file_value"
        );
    }

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

        assert_eq!(
            xattr::get(&destination, "user.keep")
                .expect("read")
                .expect("present"),
            b"keep_value"
        );
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

        assert_eq!(
            xattr::get(&destination, "user.keep")
                .expect("read")
                .expect("present"),
            b"keep_value"
        );
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
        assert!(
            xattr::get(&destination, "user.skip_this")
                .expect("read")
                .is_none(),
            "excluded xattr should not be copied"
        );
    }

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
        assert!(!destination.exists(), "dry run should not create file");
    }

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

        assert_eq!(
            fs::read(&destination).expect("read dest"),
            b"updated content"
        );
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

        let options = LocalCopyOptions::default().xattrs(true).links(true);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(summary.files_copied() >= 1);

        let copied = xattr::get(dest_root.join("real_file.txt"), "user.real_file_attr")
            .expect("read xattr")
            .expect("xattr present");
        assert_eq!(copied, b"real_value");
    }

    /// Builds a `src`/`dst` pair whose files are byte-identical and share the
    /// same mtimes, then returns the itemize records of a second `-aX` run.
    ///
    /// The second run takes the quick-check skip path, which is where upstream
    /// itemizes an up-to-date file (`generator.c:1816` -> `itemize()` with
    /// `iflags = 0`) and where an unconditional xattr flag is visible on every
    /// row.
    fn itemize_second_pass(src: &Path, dst: &Path) -> Vec<(String, bool)> {
        let operands = vec![
            OsString::from(format!("{}/", src.display())),
            dst.to_path_buf().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default()
            .recursive(true)
            .xattrs(true)
            .permissions(true)
            .times(true)
            .collect_events(true);
        let report = plan
            .execute_with_report(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");
        report
            .records()
            .iter()
            .map(|record| {
                (
                    record.relative_path().display().to_string(),
                    record.change_set().xattr_changed(),
                )
            })
            // The transfer root carries a "." row that upstream itemizes
            // separately (generator.c:1480-1483); the xattr column under test
            // belongs to the file rows.
            .filter(|(name, _)| name != ".")
            .collect()
    }

    /// Fails the caller when the second pass did not itemize both seeded files,
    /// so an assertion over "every record" can never pass vacuously.
    fn assert_both_files_itemized(records: &[(String, bool)]) {
        let mut names: Vec<&str> = records.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["a.txt", "b.txt"],
            "the up-to-date pass must itemize both files: {records:?}"
        );
    }

    /// Seeds `src` with two files carrying `user.color`, mirrors it into `dst`,
    /// and returns the pair. `dst` is produced by a first `-aX` pass so both
    /// sides agree on content, mtimes and xattrs.
    fn seeded_tree(temp: &Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let src = temp.join("src");
        let dst = temp.join("dst");
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join("a.txt"), b"alpha").expect("write a");
        fs::write(src.join("b.txt"), b"beta").expect("write b");
        if !xattrs_supported(&src.join("a.txt")) {
            eprintln!("xattrs not supported, skipping test");
            return None;
        }
        xattr::set(src.join("a.txt"), "user.color", b"red").expect("set a");
        xattr::set(src.join("b.txt"), "user.color", b"blue").expect("set b");

        seed_destination(&src, &dst);
        Some((src, dst))
    }

    /// Runs a first `-aX` pass so `dst` mirrors `src` byte-for-byte, including
    /// mtimes and xattrs.
    fn seed_destination(src: &Path, dst: &Path) {
        let operands = vec![
            OsString::from(format!("{}/", src.display())),
            dst.to_path_buf().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .recursive(true)
                .xattrs(true)
                .permissions(true)
                .times(true),
        )
        .expect("seed copy succeeds");
    }

    /// Case 1: an unchanged tree whose xattrs already match must not light the
    /// itemize `x` column anywhere.
    ///
    /// upstream: generator.c:566-572 - `itemize()` sets `ITEM_REPORT_XATTR`
    /// only when `xattr_diff()` reports a difference, and `ITEM_REPORT_XATTR`
    /// is itself part of the emit gate (generator.c:582), so a spurious flag
    /// both adds an `x` and prints a row upstream omits entirely. Verified
    /// against rsync 3.4.4 (protocol 32), which prints nothing for this tree.
    #[test]
    fn itemize_matching_xattrs_report_no_change() {
        let temp = tempdir().expect("tempdir");
        let Some((src, dst)) = seeded_tree(temp.path()) else {
            return;
        };

        let records = itemize_second_pass(&src, &dst);
        assert_both_files_itemized(&records);
        for (name, xattr_changed) in &records {
            assert!(
                !xattr_changed,
                "{name} must not report an xattr change when both sides match"
            );
        }
    }

    /// Case 2: a genuine `user.*` difference lights `x` on that file alone.
    ///
    /// Pins both halves of the comparison: the file whose value drifted reports
    /// the change, and its unchanged sibling stays silent. A flag that merely
    /// echoed `-X` would pass the first assertion and fail the second, which is
    /// exactly how the defect hid.
    #[test]
    fn itemize_differing_xattr_value_reports_only_that_file() {
        let temp = tempdir().expect("tempdir");
        let Some((src, dst)) = seeded_tree(temp.path()) else {
            return;
        };

        // Only the destination's value drifts; content and mtime are untouched,
        // so the entry still takes the quick-check skip path.
        xattr::set(dst.join("b.txt"), "user.color", b"magenta").expect("drift b");

        let records = itemize_second_pass(&src, &dst);
        assert_both_files_itemized(&records);
        let changed: Vec<_> = records
            .iter()
            .filter(|(_, xattr_changed)| *xattr_changed)
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(
            changed,
            vec!["b.txt".to_string()],
            "only the drifted file may light the x column; got {records:?}"
        );
    }

    /// A dry run itemizes the same `x` column as the real run.
    ///
    /// upstream: generator.c:1816 - the generator itemizes an up-to-date file
    /// whether or not `do_xfers` is set, so `-naXi` and `-aXi` must agree. The
    /// dry-run skip path previously hardcoded "no xattr change", which hid a
    /// genuine drift instead of inventing one.
    #[test]
    fn dry_run_itemize_matches_the_real_run() {
        let temp = tempdir().expect("tempdir");
        let Some((src, dst)) = seeded_tree(temp.path()) else {
            return;
        };
        xattr::set(dst.join("b.txt"), "user.color", b"magenta").expect("drift b");

        let operands = vec![
            OsString::from(format!("{}/", src.display())),
            dst.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let report = plan
            .execute_with_report(
                LocalCopyExecution::DryRun,
                LocalCopyOptions::default()
                    .recursive(true)
                    .xattrs(true)
                    .permissions(true)
                    .times(true)
                    .collect_events(true),
            )
            .expect("dry run succeeds");
        let changed: Vec<String> = report
            .records()
            .iter()
            .filter(|record| record.change_set().xattr_changed())
            .map(|record| record.relative_path().display().to_string())
            .collect();
        assert_eq!(
            changed,
            vec!["b.txt".to_string()],
            "a dry run must report the same x column as the real run"
        );
    }

    /// A value longer than `MAX_FULL_DATUM` that is identical on both sides
    /// must not report a change.
    ///
    /// The local-copy generator reads both lists from disk with full values,
    /// while the network generator receives an abbreviated sender list. Keying
    /// the comparison on length alone would hash the local plaintext against
    /// the destination plaintext and never match, so every large xattr would
    /// light `x` forever. upstream: xattrs.c:584-594.
    #[test]
    fn itemize_large_matching_xattr_value_reports_no_change() {
        let temp = tempdir().expect("tempdir");
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join("big.txt"), b"payload").expect("write");
        if !xattrs_supported(&src.join("big.txt")) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }
        let large = vec![b'z'; 512];
        xattr::set(src.join("big.txt"), "user.large", &large).expect("set large");

        seed_destination(&src, &dst);

        let records = itemize_second_pass(&src, &dst);
        assert_eq!(
            records.len(),
            1,
            "the up-to-date pass must itemize the file: {records:?}"
        );
        for (name, xattr_changed) in &records {
            assert!(
                !xattr_changed,
                "{name} carries an identical large xattr and must stay silent"
            );
        }
    }
}
