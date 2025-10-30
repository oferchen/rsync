#[test]
fn run_client_delete_respects_dry_run() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_root.clone().into_os_string()])
        .dry_run(true)
        .delete(true)
        .build();

    let summary = run_client(config).expect("dry-run succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"stale"
    );
    assert!(dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn run_client_update_skips_newer_destination() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-update.txt");
    let destination = tmp.path().join("dest-update.txt");
    fs::write(&source, b"fresh").expect("write source");
    fs::write(&destination, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older, older).expect("set source times");
    set_file_times(&destination, newer, newer).expect("set dest times");

    let summary = run_client(
        ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                destination.clone().into_os_string(),
            ])
            .update(true)
            .build(),
    )
    .expect("run client");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn run_client_respects_filter_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([FilterRuleSpec::exclude("*.tmp".to_string())])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert!(dest_root.join("source").join("keep.txt").exists());
    assert!(!dest_root.join("source").join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn run_client_filter_clear_resets_previous_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([
            FilterRuleSpec::exclude("*.tmp".to_string()),
            FilterRuleSpec::clear(),
            FilterRuleSpec::exclude("keep.txt".to_string()),
        ])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("keep.txt").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn run_client_copies_directory_tree() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    let source_file = nested.join("file.txt");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(&source_file, b"tree").expect("write source file");

    let dest_root = tmp.path().join("destination");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_file = dest_root.join("nested").join("file.txt");
    assert_eq!(fs::read(copied_file).expect("read copied"), b"tree");
    assert!(summary.files_copied() >= 1);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn run_client_copies_symbolic_link() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"symlink target").expect("write target");

    let source_link = tmp.path().join("source-link");
    symlink(&target_file, &source_link).expect("create source symlink");

    let destination_link = tmp.path().join("dest-link");
    let config = ClientConfig::builder()
        .transfer_args([source_link.clone(), destination_link.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("link copy succeeds");

    let copied = fs::read_link(destination_link).expect("read copied link");
    assert_eq!(copied, target_file);
    assert_eq!(summary.symlinks_copied(), 1);

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
        .expect("symlink event present");
    let recorded_target = event
        .metadata()
        .and_then(ClientEntryMetadata::symlink_target)
        .expect("symlink target recorded");
    assert_eq!(recorded_target, target_file.as_path());
}

#[cfg(unix)]
#[test]
fn run_client_preserves_symbolic_links_in_directories() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");

    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"data").expect("write target");
    let link_path = nested.join("link");
    symlink(&target_file, &link_path).expect("create link");

    let dest_root = tmp.path().join("destination");
    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_link = dest_root.join("nested").join("link");
    let copied_target = fs::read_link(copied_link).expect("read copied link");
    assert_eq!(copied_target, target_file);
    assert_eq!(summary.symlinks_copied(), 1);

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
        .expect("symlink event present");
    let recorded_target = event
        .metadata()
        .and_then(ClientEntryMetadata::symlink_target)
        .expect("symlink target recorded");
    assert_eq!(recorded_target, target_file.as_path());
}

#[cfg(unix)]
#[test]
fn run_client_preserves_file_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-metadata.txt");
    let destination = tmp.path().join("dest-metadata.txt");
    fs::write(&source, b"metadata").expect("write source");

    let mode = 0o640;
    fs::set_permissions(&source, PermissionsExt::from_mode(mode)).expect("set source permissions");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set source timestamps");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    assert_eq!(source_metadata.permissions().mode() & 0o777, mode);
    let src_atime = FileTime::from_last_access_time(&source_metadata);
    let src_mtime = FileTime::from_last_modification_time(&source_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn run_client_preserves_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-dir");
    fs::create_dir(&source_dir).expect("create source dir");

    let mode = 0o751;
    fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
        .expect("set directory permissions");
    let atime = FileTime::from_unix_time(1_700_010_000, 0);
    let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
    set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

    let destination_dir = tmp.path().join("dest-dir");
    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), destination_dir.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("directory copy succeeds");

    let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
    assert!(dest_metadata.is_dir());
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn run_client_updates_existing_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-tree");
    let source_nested = source_dir.join("nested");
    fs::create_dir_all(&source_nested).expect("create source tree");

    let source_mode = 0o745;
    fs::set_permissions(&source_nested, PermissionsExt::from_mode(source_mode))
        .expect("set source nested permissions");
    let source_atime = FileTime::from_unix_time(1_700_030_000, 1_000_000);
    let source_mtime = FileTime::from_unix_time(1_700_040_000, 2_000_000);
    set_file_times(&source_nested, source_atime, source_mtime)
        .expect("set source nested timestamps");

    let dest_root = tmp.path().join("dest-root");
    fs::create_dir(&dest_root).expect("create dest root");
    let dest_dir = dest_root.join("source-tree");
    let dest_nested = dest_dir.join("nested");
    fs::create_dir_all(&dest_nested).expect("pre-create destination tree");

    let dest_mode = 0o711;
    fs::set_permissions(&dest_nested, PermissionsExt::from_mode(dest_mode))
        .expect("set dest nested permissions");
    let dest_atime = FileTime::from_unix_time(1_600_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_600_100_000, 0);
    set_file_times(&dest_nested, dest_atime, dest_mtime).expect("set dest nested timestamps");

    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), dest_root.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let _summary = run_client(config).expect("directory copy succeeds");

    let copied_nested = dest_root.join("source-tree").join("nested");
    let copied_metadata = fs::metadata(&copied_nested).expect("dest metadata");
    assert!(copied_metadata.is_dir());
    assert_eq!(copied_metadata.permissions().mode() & 0o777, source_mode);
    let copied_atime = FileTime::from_last_access_time(&copied_metadata);
    let copied_mtime = FileTime::from_last_modification_time(&copied_metadata);
    assert_eq!(copied_atime, source_atime);
    assert_eq!(copied_mtime, source_mtime);
}

#[cfg(unix)]
#[test]
fn run_client_sparse_copy_creates_holes() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sparse-source.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x11]).expect("write leading");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x22]).expect("write middle");
    source_file
        .seek(SeekFrom::Start(4 * 1024 * 1024))
        .expect("seek to tail");
    source_file.write_all(&[0x33]).expect("write tail");
    source_file.set_len(6 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let dense_config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .build();
    let summary = run_client(dense_config).expect("dense copy succeeds");
    assert!(summary.events().is_empty());

    let sparse_config = ClientConfig::builder()
        .transfer_args([
            source.into_os_string(),
            sparse_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .sparse(true)
        .build();
    let summary = run_client(sparse_config).expect("sparse copy succeeds");
    assert!(summary.events().is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

#[test]
fn run_client_merges_directory_contents_when_trailing_separator_present() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    let file_path = nested.join("file.txt");
    fs::write(&file_path, b"contents").expect("write file");

    let dest_root = tmp.path().join("dest");
    let mut source_arg = source_root.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .build();

    let summary = run_client(config).expect("directory contents copy succeeds");

    assert!(dest_root.is_dir());
    assert!(dest_root.join("nested").is_dir());
    assert_eq!(
        fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
        b"contents"
    );
    assert!(!dest_root.join("source").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn module_list_request_detects_remote_url() {
    let operands = vec![OsString::from("rsync://example.com:8730/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 8730);
}

#[test]
fn module_list_request_accepts_mixed_case_scheme() {
    let operands = vec![OsString::from("RSyNc://Example.COM/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "Example.COM");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_honours_custom_default_port() {
    let operands = vec![OsString::from("rsync://example.com/")];
    let request = ModuleListRequest::from_operands_with_port(&operands, 10_873)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 10_873);
}

#[test]
fn module_list_request_rejects_remote_transfer() {
    let operands = vec![OsString::from("rsync://example.com/module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}

#[test]
fn module_list_request_accepts_username_in_rsync_url() {
    let operands = vec![OsString::from("rsync://user@example.com/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_request_accepts_username_in_legacy_syntax() {
    let operands = vec![OsString::from("user@example.com::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_request_supports_ipv6_in_rsync_url() {
    let operands = vec![OsString::from("rsync://[2001:db8::1]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "2001:db8::1");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_ipv6_in_legacy_syntax() {
    let operands = vec![OsString::from("[fe80::1]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1");
    assert_eq!(request.address().port(), 873);
}

