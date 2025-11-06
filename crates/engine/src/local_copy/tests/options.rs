
#[test]
fn local_copy_options_numeric_ids_round_trip() {
    let options = LocalCopyOptions::default().numeric_ids(true);
    assert!(options.numeric_ids_enabled());
    assert!(!LocalCopyOptions::default().numeric_ids_enabled());
}

#[test]
fn local_copy_options_delete_excluded_round_trip() {
    let options = LocalCopyOptions::default().delete_excluded(true);
    assert!(options.delete_excluded_enabled());
    assert!(!LocalCopyOptions::default().delete_excluded_enabled());
}

#[test]
fn local_copy_options_delete_delay_round_trip() {
    let options = LocalCopyOptions::default().delete_delay(true);
    assert!(options.delete_delay_enabled());
    assert!(!LocalCopyOptions::default().delete_delay_enabled());
}

#[test]
fn local_copy_options_modify_window_round_trip() {
    let window = Duration::from_secs(5);
    let options = LocalCopyOptions::default().with_modify_window(window);
    assert_eq!(options.modify_window(), window);
    assert_eq!(LocalCopyOptions::default().modify_window(), Duration::ZERO);
}

#[test]
fn should_skip_copy_requires_exact_mtime_when_window_zero() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let destination = temp.path().join("dest");
    fs::write(&source, b"payload").expect("write source");
    fs::write(&destination, b"payload").expect("write dest");

    let base = FileTime::from_unix_time(1_700_000_000, 0);
    let offset = FileTime::from_unix_time(1_700_000_003, 0);
    set_file_mtime(&source, base).expect("set source mtime");
    set_file_mtime(&destination, offset).expect("set dest mtime");

    let source_meta = fs::metadata(&source).expect("source metadata");
    let dest_meta = fs::metadata(&destination).expect("dest metadata");
    let metadata_options = MetadataOptions::new().preserve_times(true);

    assert!(!should_skip_copy(CopyComparison {
        source_path: &source,
        source: &source_meta,
        destination_path: &destination,
        destination: &dest_meta,
        options: &metadata_options,
        size_only: false,
        checksum: false,
        checksum_algorithm: SignatureAlgorithm::Md5,
        modify_window: Duration::ZERO,
    }));
}

#[test]
fn should_skip_copy_allows_difference_within_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let destination = temp.path().join("dest");
    fs::write(&source, b"payload").expect("write source");
    fs::write(&destination, b"payload").expect("write dest");

    let base = FileTime::from_unix_time(1_700_000_100, 0);
    let within = FileTime::from_unix_time(1_700_000_103, 0);
    let outside = FileTime::from_unix_time(1_700_000_107, 0);
    set_file_mtime(&source, base).expect("set source mtime");
    set_file_mtime(&destination, within).expect("set dest mtime");

    let metadata_options = MetadataOptions::new().preserve_times(true);

    let source_meta = fs::metadata(&source).expect("source metadata");
    let mut dest_meta = fs::metadata(&destination).expect("dest metadata");

    assert!(should_skip_copy(CopyComparison {
        source_path: &source,
        source: &source_meta,
        destination_path: &destination,
        destination: &dest_meta,
        options: &metadata_options,
        size_only: false,
        checksum: false,
        checksum_algorithm: SignatureAlgorithm::Md5,
        modify_window: Duration::from_secs(5),
    }));

    set_file_mtime(&destination, outside).expect("set dest outside window");
    dest_meta = fs::metadata(&destination).expect("dest metadata outside");

    assert!(!should_skip_copy(CopyComparison {
        source_path: &source,
        source: &source_meta,
        destination_path: &destination,
        destination: &dest_meta,
        options: &metadata_options,
        size_only: false,
        checksum: false,
        checksum_algorithm: SignatureAlgorithm::Md5,
        modify_window: Duration::from_secs(5),
    }));
}

#[test]
fn local_copy_options_backup_round_trip() {
    let options = LocalCopyOptions::default().backup(true);
    assert!(options.backup_enabled());
    assert!(!LocalCopyOptions::default().backup_enabled());
}

#[test]
fn local_copy_options_backup_directory_enables_backup() {
    let options = LocalCopyOptions::default().with_backup_directory(Some(PathBuf::from("backups")));
    assert!(options.backup_enabled());
    assert_eq!(
        options.backup_directory(),
        Some(std::path::Path::new("backups"))
    );
    assert!(LocalCopyOptions::default().backup_directory().is_none());
}

#[test]
fn local_copy_options_backup_suffix_round_trip() {
    let options = LocalCopyOptions::default().with_backup_suffix(Some(".bak"));
    assert!(options.backup_enabled());
    assert_eq!(options.backup_suffix(), OsStr::new(".bak"));
    assert_eq!(LocalCopyOptions::default().backup_suffix(), OsStr::new("~"));
    assert!(!LocalCopyOptions::default().backup_enabled());
}

#[test]
fn local_copy_options_checksum_round_trip() {
    let options = LocalCopyOptions::default().checksum(true);
    assert!(options.checksum_enabled());
    assert!(!LocalCopyOptions::default().checksum_enabled());
}

#[test]
fn local_copy_options_checksum_algorithm_round_trip() {
    let options =
        LocalCopyOptions::default().with_checksum_algorithm(SignatureAlgorithm::Xxh3 { seed: 42 });
    assert_eq!(
        options.checksum_algorithm(),
        SignatureAlgorithm::Xxh3 { seed: 42 }
    );
    assert_eq!(
        LocalCopyOptions::default().checksum_algorithm(),
        SignatureAlgorithm::Md5
    );
}

#[test]
fn local_copy_options_omit_dir_times_round_trip() {
    let options = LocalCopyOptions::default().omit_dir_times(true);
    assert!(options.omit_dir_times_enabled());
    assert!(!LocalCopyOptions::default().omit_dir_times_enabled());
}

#[test]
fn files_checksum_match_respects_algorithm_selection() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, b"payload").expect("write source");
    fs::write(&destination, b"payload").expect("write destination");

    let algorithms = [
        SignatureAlgorithm::Md4,
        SignatureAlgorithm::Md5,
        SignatureAlgorithm::Sha1,
        SignatureAlgorithm::Xxh64 { seed: 7 },
        SignatureAlgorithm::Xxh3 { seed: 13 },
        SignatureAlgorithm::Xxh3_128 { seed: 0 },
    ];

    for &algorithm in &algorithms {
        assert!(files_checksum_match(&source, &destination, algorithm).expect("hash ok"));
    }

    fs::write(&destination, b"payloae").expect("write differing payload");

    for &algorithm in &algorithms {
        assert!(!files_checksum_match(&source, &destination, algorithm).expect("hash ok"));
    }
}

#[test]
fn local_copy_options_omit_link_times_round_trip() {
    let options = LocalCopyOptions::default().omit_link_times(true);
    assert!(options.omit_link_times_enabled());
    assert!(!LocalCopyOptions::default().omit_link_times_enabled());
}

#[test]
fn metadata_options_reflect_numeric_ids_setting() {
    let options = LocalCopyOptions::default().numeric_ids(true);
    let context = CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));
    assert!(context.metadata_options().numeric_ids_enabled());
}

#[test]
fn metadata_options_carry_chmod_modifiers() {
    let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
    let options = LocalCopyOptions::default().with_chmod(Some(modifiers.clone()));
    let context = CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));

    assert_eq!(context.metadata_options().chmod(), Some(&modifiers));
}

#[test]
fn symlink_target_is_safe_accepts_relative_paths() {
    assert!(symlink_target_is_safe(
        Path::new("target"),
        Path::new("dir")
    ));
    assert!(symlink_target_is_safe(
        Path::new("nested/entry"),
        Path::new("dir/subdir")
    ));
    assert!(symlink_target_is_safe(
        Path::new("./inner"),
        Path::new("dest")
    ));
}

#[test]
fn symlink_target_is_safe_rejects_unsafe_targets() {
    assert!(!symlink_target_is_safe(Path::new(""), Path::new("dest")));
    assert!(!symlink_target_is_safe(
        Path::new("/etc/passwd"),
        Path::new("dest")
    ));
    assert!(!symlink_target_is_safe(
        Path::new("../../escape"),
        Path::new("dest")
    ));
    assert!(!symlink_target_is_safe(
        Path::new("../../../escape"),
        Path::new("dest/subdir")
    ));
    assert!(!symlink_target_is_safe(
        Path::new("dir/.."),
        Path::new("dest")
    ));
    assert!(!symlink_target_is_safe(
        Path::new("dir/../.."),
        Path::new("dest")
    ));
}

#[test]
fn local_copy_options_sparse_round_trip() {
    let options = LocalCopyOptions::default().sparse(true);
    assert!(options.sparse_enabled());
    assert!(!LocalCopyOptions::default().sparse_enabled());
}

#[test]
fn local_copy_options_append_round_trip() {
    let options = LocalCopyOptions::default().append(true);
    assert!(options.append_enabled());
    assert!(!options.append_verify_enabled());
    assert!(!LocalCopyOptions::default().append_enabled());
}

#[test]
fn local_copy_options_append_verify_round_trip() {
    let options = LocalCopyOptions::default().append_verify(true);
    assert!(options.append_enabled());
    assert!(options.append_verify_enabled());
    let disabled = options.append(false);
    assert!(!disabled.append_enabled());
    assert!(!disabled.append_verify_enabled());
}

#[test]
fn local_copy_options_preallocate_round_trip() {
    let options = LocalCopyOptions::default().preallocate(true);
    assert!(options.preallocate_enabled());
    assert!(!LocalCopyOptions::default().preallocate_enabled());
}

#[test]
fn preallocate_destination_reserves_space() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("prealloc.bin");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .expect("create file");

    maybe_preallocate_destination(&mut file, &path, 4096, 0, true).expect("preallocate file");

    let metadata = fs::metadata(&path).expect("metadata");
    assert_eq!(metadata.len(), 4096);
}

#[test]
fn local_copy_options_compress_round_trip() {
    let options = LocalCopyOptions::default().compress(true);
    assert!(options.compress_enabled());
    assert!(!LocalCopyOptions::default().compress_enabled());
}

#[test]
fn local_copy_options_whole_file_round_trip() {
    assert!(LocalCopyOptions::default().whole_file_enabled());
    let delta = LocalCopyOptions::default().whole_file(false);
    assert!(!delta.whole_file_enabled());
    let restored = delta.whole_file(true);
    assert!(restored.whole_file_enabled());
}

#[test]
fn local_copy_options_copy_links_round_trip() {
    let options = LocalCopyOptions::default().copy_links(true);
    assert!(options.copy_links_enabled());
    assert!(!LocalCopyOptions::default().copy_links_enabled());
}

#[test]
fn local_copy_options_timeout_round_trip() {
    let timeout = Duration::from_secs(5);
    let options = LocalCopyOptions::default().with_timeout(Some(timeout));
    assert_eq!(options.timeout(), Some(timeout));
    assert!(LocalCopyOptions::default().timeout().is_none());
}

#[test]
fn local_copy_options_compression_override_round_trip() {
    let level = NonZeroU8::new(5).expect("level");
    let override_level = CompressionLevel::precise(level);
    let options = LocalCopyOptions::default()
        .compress(true)
        .with_compression_level_override(Some(override_level));
    assert_eq!(options.compression_level_override(), Some(override_level));
    assert_eq!(options.compression_level(), override_level);
    assert_eq!(options.effective_compression_level(), Some(override_level));

    let disabled = LocalCopyOptions::default()
        .with_compression_level_override(Some(override_level))
        .compress(false);
    assert_eq!(disabled.compression_level_override(), None);
    assert_eq!(disabled.compression_level(), CompressionLevel::Default);
    assert_eq!(disabled.effective_compression_level(), None);
}

#[test]
fn local_copy_options_compression_level_round_trip() {
    let options =
        LocalCopyOptions::default().with_default_compression_level(CompressionLevel::Best);
    assert_eq!(options.compression_level(), CompressionLevel::Best);
    assert_eq!(
        LocalCopyOptions::default().compression_level(),
        CompressionLevel::Default
    );
}

#[test]
fn local_copy_options_size_only_round_trip() {
    let options = LocalCopyOptions::default().size_only(true);
    assert!(options.size_only_enabled());
    assert!(!LocalCopyOptions::default().size_only_enabled());
}

#[test]
fn local_copy_options_ignore_existing_round_trip() {
    let options = LocalCopyOptions::default().ignore_existing(true);
    assert!(options.ignore_existing_enabled());
    assert!(!LocalCopyOptions::default().ignore_existing_enabled());
}

#[test]
fn local_copy_options_ignore_missing_args_round_trip() {
    let options = LocalCopyOptions::default().ignore_missing_args(true);
    assert!(options.ignore_missing_args_enabled());
    assert!(!LocalCopyOptions::default().ignore_missing_args_enabled());
}

#[test]
fn local_copy_options_update_round_trip() {
    let options = LocalCopyOptions::default().update(true);
    assert!(options.update_enabled());
    assert!(!LocalCopyOptions::default().update_enabled());
}

#[test]
fn local_copy_options_delay_updates_round_trip() {
    let options = LocalCopyOptions::default().delay_updates(true);
    assert!(options.delay_updates_enabled());
    assert!(!LocalCopyOptions::default().delay_updates_enabled());
}

#[test]
fn local_copy_options_devices_round_trip() {
    let options = LocalCopyOptions::default().devices(true);
    assert!(options.devices_enabled());
    assert!(!LocalCopyOptions::default().devices_enabled());
}

#[test]
fn local_copy_options_specials_round_trip() {
    let options = LocalCopyOptions::default().specials(true);
    assert!(options.specials_enabled());
    assert!(!LocalCopyOptions::default().specials_enabled());
}

#[test]
fn local_copy_options_relative_round_trip() {
    let options = LocalCopyOptions::default().relative_paths(true);
    assert!(options.relative_paths_enabled());
    assert!(!LocalCopyOptions::default().relative_paths_enabled());
}

#[test]
fn local_copy_options_implied_dirs_round_trip() {
    let options = LocalCopyOptions::default();
    assert!(options.implied_dirs_enabled());

    let disabled = LocalCopyOptions::default().implied_dirs(false);
    assert!(!disabled.implied_dirs_enabled());

    let reenabled = disabled.implied_dirs(true);
    assert!(reenabled.implied_dirs_enabled());
}

#[test]
fn local_copy_options_mkpath_round_trip() {
    let options = LocalCopyOptions::default();
    assert!(!options.mkpath_enabled());

    let enabled = LocalCopyOptions::default().mkpath(true);
    assert!(enabled.mkpath_enabled());

    let disabled = enabled.mkpath(false);
    assert!(!disabled.mkpath_enabled());
}

#[test]
fn local_copy_options_temp_directory_round_trip() {
    let options = LocalCopyOptions::default().with_temp_directory(Some(PathBuf::from(".temp")));
    assert_eq!(options.temp_directory_path(), Some(Path::new(".temp")));

    let cleared = options.with_temp_directory::<PathBuf>(None);
    assert!(cleared.temp_directory_path().is_none());
}
