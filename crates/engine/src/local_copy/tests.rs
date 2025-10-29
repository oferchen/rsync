#![cfg(test)]

use super::*;
use filetime::{FileTime, set_file_mtime, set_file_times};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::Path;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[cfg(feature = "xattr")]
use xattr;

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::convert::TryInto;

    let bits: u16 = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let mode = Mode::from_bits_truncate(bits.into());
    mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use std::convert::TryInto;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))?;
    let result = unsafe { libc::mkfifo(path_c.as_ptr(), bits) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, feature = "acl"))]
use std::os::unix::ffi::OsStrExt;

#[cfg(all(unix, feature = "acl"))]
mod acl_sys {
    #![allow(unsafe_code)]

    use libc::{c_char, c_int, c_void, ssize_t};

    pub type AclHandle = *mut c_void;
    pub type AclType = c_int;

    pub const ACL_TYPE_ACCESS: AclType = 0x8000;

    #[link(name = "acl")]
    unsafe extern "C" {
        pub fn acl_get_file(path_p: *const c_char, ty: AclType) -> AclHandle;
        pub fn acl_set_file(path_p: *const c_char, ty: AclType, acl: AclHandle) -> c_int;
        pub fn acl_to_text(acl: AclHandle, len_p: *mut ssize_t) -> *mut c_char;
        pub fn acl_from_text(buf_p: *const c_char) -> AclHandle;
        pub fn acl_free(obj_p: *mut c_void) -> c_int;
    }

    pub unsafe fn free(handle: AclHandle) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle) };
    }

    pub unsafe fn free_text(handle: *mut c_char) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle.cast()) };
    }
}

#[cfg(all(unix, feature = "acl"))]
fn acl_to_text(path: &Path, ty: acl_sys::AclType) -> Option<String> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
    let acl = unsafe { acl_sys::acl_get_file(c_path.as_ptr(), ty) };
    if acl.is_null() {
        return None;
    }
    let mut len = 0;
    let text_ptr = unsafe { acl_sys::acl_to_text(acl, &mut len) };
    if text_ptr.is_null() {
        unsafe { acl_sys::free(acl) };
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
    let text = String::from_utf8_lossy(slice).trim().to_string();
    unsafe {
        acl_sys::free_text(text_ptr);
        acl_sys::free(acl);
    }
    Some(text)
}

#[cfg(all(unix, feature = "acl"))]
fn set_acl_from_text(path: &Path, text: &str, ty: acl_sys::AclType) {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
    let c_text = std::ffi::CString::new(text).expect("text");
    let acl = unsafe { acl_sys::acl_from_text(c_text.as_ptr()) };
    assert!(!acl.is_null(), "acl_from_text");
    let result = unsafe { acl_sys::acl_set_file(c_path.as_ptr(), ty, acl) };
    unsafe {
        acl_sys::free(acl);
    }
    assert_eq!(result, 0, "acl_set_file");
}

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

#[test]
fn parse_filter_directive_show_is_sender_only() {
    let rule = match parse_filter_directive_line("show images/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_hide_is_sender_only() {
    let rule = match parse_filter_directive_line("hide *.tmp").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_show_is_sender_only() {
    let rule = match parse_filter_directive_line("S logs/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_hide_is_sender_only() {
    let rule = match parse_filter_directive_line("H *.bak").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_protect_requires_receiver() {
    let rule = match parse_filter_directive_line("P cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_risk_requires_receiver() {
    let rule = match parse_filter_directive_line("risk cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_risk_requires_receiver() {
    let rule = match parse_filter_directive_line("R cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {:?}", other),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_clear_keyword() {
    let directive = parse_filter_directive_line("clear").expect("parse clear");
    assert!(matches!(directive, Some(ParsedFilterDirective::Clear)));

    let uppercase = parse_filter_directive_line("  CLEAR  ").expect("parse uppercase");
    assert!(matches!(uppercase, Some(ParsedFilterDirective::Clear)));

    let bang = parse_filter_directive_line("!").expect("parse bang");
    assert!(matches!(bang, Some(ParsedFilterDirective::Clear)));
}

#[test]
fn parse_filter_directive_exclude_if_present_support() {
    let directive = parse_filter_directive_line("exclude-if-present=.git")
        .expect("parse")
        .expect("directive");

    match directive {
        ParsedFilterDirective::ExcludeIfPresent(rule) => {
            assert_eq!(rule.marker_path(Path::new(".")), PathBuf::from("./.git"));
        }
        other => panic!("expected exclude-if-present directive, got {:?}", other),
    }
}

#[test]
fn parse_filter_directive_dir_merge_without_modifiers() {
    let directive = parse_filter_directive_line("dir-merge .rsync-filter")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected dir-merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from(".rsync-filter"));
    let opts = options.expect("options");
    assert!(opts.inherit_rules());
    assert!(opts.allows_comments());
    assert!(!opts.uses_whitespace());
    assert_eq!(opts.enforced_kind(), None);
}

#[test]
fn parse_filter_directive_dir_merge_with_modifiers() {
    let directive = parse_filter_directive_line("dir-merge,+ne rules/filter.txt")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected dir-merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from("rules/filter.txt"));
    let opts = options.expect("options");
    assert!(!opts.inherit_rules());
    assert!(opts.excludes_self());
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
}

#[test]
fn parse_filter_directive_dir_merge_cvs_default_path() {
    let directive = parse_filter_directive_line("dir-merge,c")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected dir-merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from(".cvsignore"));
    let opts = options.expect("options");
    assert!(!opts.inherit_rules());
    assert!(opts.list_clear_allowed());
    assert!(opts.uses_whitespace());
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_short_merge_inherits_context() {
    let directive = parse_filter_directive_line(". per-dir")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from("per-dir"));
    assert!(options.is_none());
}

#[test]
fn parse_filter_directive_short_merge_cvs_defaults() {
    let directive = parse_filter_directive_line(".C")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from(".cvsignore"));
    let opts = options.expect("options");
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    assert!(opts.uses_whitespace());
    assert!(!opts.inherit_rules());
}

#[test]
fn parse_filter_directive_short_dir_merge_with_modifiers() {
    let directive = parse_filter_directive_line(":- per-dir")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected dir-merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from("per-dir"));
    let opts = options.expect("options");
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_merge_with_modifiers() {
    let directive = parse_filter_directive_line("merge,+ rules")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {:?}", other),
    };

    assert_eq!(path, PathBuf::from("rules"));
    let opts = options.expect("options");
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
}

#[test]
fn parse_filter_directive_dir_merge_conflicting_modifiers_error() {
    let error = parse_filter_directive_line("dir-merge,+- rules").expect_err("conflict");
    assert!(
        error
            .to_string()
            .contains("cannot combine '+' and '-' modifiers")
    );
}

#[test]
fn deferred_updates_flush_commits_pending_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"payload").expect("write source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create dest root");
    let destination = destination_root.join("file.txt");

    let options = LocalCopyOptions::default()
        .partial(true)
        .delay_updates(true);
    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        options,
        None,
        destination_root.clone(),
    );

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), true, None, None).expect("guard");
    file.write_all(b"payload").expect("write temp");
    drop(file);

    let metadata = fs::metadata(&source).expect("metadata");
    let metadata_options = context.metadata_options();
    let partial_path = partial_destination_path(&destination);
    let final_path = guard.final_path().to_path_buf();
    let update = DeferredUpdate {
        guard,
        metadata: metadata.clone(),
        metadata_options,
        mode: LocalCopyExecution::Apply,
        source: source.clone(),
        relative: Some(std::path::PathBuf::from("file.txt")),
        destination: final_path,
        file_type: metadata.file_type(),
        destination_previously_existed: false,
        #[cfg(feature = "xattr")]
        preserve_xattrs: context.xattrs_enabled(),
        #[cfg(feature = "acl")]
        preserve_acls: context.acls_enabled(),
    };

    context.register_deferred_update(update);

    assert!(!destination.exists());
    assert!(partial_path.exists());

    context
        .flush_deferred_updates()
        .expect("deferred updates committed");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
    assert!(!partial_path.exists());
}

#[test]
fn dir_merge_defaults_preserve_rule_side_overrides() {
    let options = DirMergeOptions::default();
    let rule = FilterRule::exclude("*.tmp").with_receiver(false);
    let adjusted = apply_dir_merge_rule_defaults(rule, &options);

    assert!(adjusted.applies_to_sender());
    assert!(!adjusted.applies_to_receiver());
}

#[test]
fn dir_merge_modifiers_override_rule_side_overrides() {
    let sender_only_options = DirMergeOptions::default().sender_modifier();
    let receiver_only_options = DirMergeOptions::default().receiver_modifier();

    let rule = FilterRule::include("logs/**").with_receiver(false);
    let sender_adjusted = apply_dir_merge_rule_defaults(rule.clone(), &sender_only_options);
    assert!(sender_adjusted.applies_to_sender());
    assert!(!sender_adjusted.applies_to_receiver());

    let receiver_adjusted = apply_dir_merge_rule_defaults(rule, &receiver_only_options);
    assert!(!receiver_adjusted.applies_to_sender());
    assert!(receiver_adjusted.applies_to_receiver());
}

#[test]
fn relative_root_drops_absolute_prefix_without_marker() {
    let operand = OsString::from("/var/log/messages");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    let expected = Path::new("var").join("log").join("messages");
    assert_eq!(spec.relative_root(), Some(expected));
}

#[test]
fn relative_root_respects_marker_boundary() {
    let operand = OsString::from("/srv/./data/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("data/file.txt").to_path_buf())
    );
}

#[test]
fn relative_root_keeps_relative_paths_without_marker() {
    let operand = OsString::from("nested/dir/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("nested/dir/file.txt").to_path_buf())
    );
}

#[test]
fn relative_root_counts_parent_components_before_marker() {
    let operand = OsString::from("dir/.././trimmed/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("trimmed/file.txt").to_path_buf())
    );
}

#[cfg(windows)]
#[test]
fn relative_root_handles_windows_drive_prefix() {
    let operand = OsString::from(r"C:\\path\\.\\to\\file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("to/file.txt").to_path_buf())
    );
}

#[cfg(feature = "xattr")]
#[test]
fn local_copy_options_xattrs_round_trip() {
    let options = LocalCopyOptions::default().xattrs(true);
    assert!(options.preserve_xattrs());
    assert!(
        !LocalCopyOptions::default()
            .xattrs(true)
            .xattrs(false)
            .preserve_xattrs()
    );
}

#[cfg(unix)]
mod unix_ids {
    #![allow(unsafe_code)]

    pub(super) fn uid(raw: u32) -> rustix::fs::Uid {
        unsafe { rustix::fs::Uid::from_raw(raw) }
    }

    pub(super) fn gid(raw: u32) -> rustix::fs::Gid {
        unsafe { rustix::fs::Gid::from_raw(raw) }
    }
}

#[test]
fn plan_from_operands_requires_destination() {
    let operands = vec![OsString::from("only-source")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("missing destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::MissingSourceOperands
    ));
}

#[test]
fn plan_rejects_empty_operands() {
    let operands = vec![OsString::new(), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("empty source");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptySourceOperand)
    ));
}

#[test]
fn plan_rejects_empty_destination() {
    let operands = vec![OsString::from("src"), OsString::new()];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("empty destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptyDestinationOperand)
    ));
}

#[test]
fn plan_rejects_remote_module_source() {
    let operands = vec![OsString::from("host::module"), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote module");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_rejects_remote_shell_source() {
    let operands = vec![OsString::from("host:/path"), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote shell source");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_rejects_remote_destination() {
    let operands = vec![OsString::from("src"), OsString::from("rsync://host/module")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_accepts_windows_drive_style_paths() {
    let operands = vec![OsString::from("C:\\source"), OsString::from("C:\\dest")];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan accepts drive paths");
    assert_eq!(plan.sources().len(), 1);
}

#[test]
fn plan_detects_trailing_separator() {
    let operands = vec![OsString::from("dir/"), OsString::from("dest")];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    assert!(plan.sources()[0].copy_contents());
}

#[test]
fn execute_creates_directory_for_trailing_destination_separator() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"payload").expect("write source");

    let dest_dir = temp.path().join("dest");
    let mut destination_operand = dest_dir.clone().into_os_string();
    destination_operand.push(std::path::MAIN_SEPARATOR_STR);

    let operands = vec![source.clone().into_os_string(), destination_operand];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let copied = dest_dir.join(source.file_name().expect("source name"));
    assert_eq!(fs::read(copied).expect("read copied"), b"payload");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_copies_single_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"example").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), b"example");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

struct SleepyHandler {
    slept: bool,
    delay: Duration,
}

impl LocalCopyRecordHandler for SleepyHandler {
    fn handle(&mut self, _record: LocalCopyRecord) {
        if !self.slept {
            self.slept = true;
            thread::sleep(self.delay);
        }
    }
}

#[test]
fn local_copy_timeout_enforced() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, vec![0u8; 1024]).expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let mut handler = SleepyHandler {
        slept: false,
        delay: Duration::from_millis(50),
    };

    let options = LocalCopyOptions::default().with_timeout(Some(Duration::from_millis(5)));
    let error = plan
        .execute_with_options_and_handler(LocalCopyExecution::Apply, options, Some(&mut handler))
        .expect_err("timeout should fail copy");

    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
    assert!(
        !destination.exists(),
        "destination should not be created on timeout"
    );
}

#[test]
fn execute_with_remove_source_files_deletes_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed");
    assert_eq!(fs::read(destination).expect("read dest"), b"move me");
}

#[test]
fn execute_with_remove_source_files_preserves_unchanged_source() {
    use filetime::{FileTime, set_file_times};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let payload = b"stable";
    fs::write(&source, payload).expect("write source");
    fs::write(&destination, payload).expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set dest times");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .times(true),
        )
        .expect("execution succeeds");

    assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
    assert!(source.exists(), "source should remain when unchanged");
    assert!(destination.exists(), "destination remains present");
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn execute_with_relative_preserves_parent_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let source_file = source_root.join("foo").join("bar").join("nested.txt");
    fs::write(&source_file, b"relative").expect("write source");

    let operand = source_root
        .join(".")
        .join("foo")
        .join("bar")
        .join("nested.txt");

    let operands = vec![
        operand.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().relative_paths(true),
        )
        .expect("copy succeeds");

    let copied = destination_root.join("foo").join("bar").join("nested.txt");
    assert_eq!(fs::read(copied).expect("read copied"), b"relative");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_relative_requires_directory_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(source_root.join("dir")).expect("create source tree");
    let source_file = source_root.join("dir").join("file.txt");
    fs::write(&source_file, b"dir").expect("write source");

    let destination = temp.path().join("dest.txt");
    fs::write(&destination, b"target").expect("write destination");

    let operand = source_root.join(".").join("dir").join("file.txt");

    let operands = vec![
        operand.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().relative_paths(true),
    );

    let error = result.expect_err("relative paths require directory destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::DestinationMustBeDirectory)
    ));
    assert_eq!(fs::read(&destination).expect("read destination"), b"target");
}

#[cfg(feature = "xattr")]
#[test]
fn execute_copies_file_with_xattrs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"attr").expect("write source");
    xattr::set(&source, "user.demo", b"value").expect("set xattr");

    let operands = vec![
        source.clone().into_os_string(),
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
    let copied = xattr::get(&destination, "user.demo")
        .expect("read dest xattr")
        .expect("xattr present");
    assert_eq!(copied, b"value");
}

#[cfg(all(unix, feature = "acl"))]
#[test]
fn execute_copies_file_with_acls() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"acl").expect("write source");
    let acl_text = "user::rw-\ngroup::r--\nother::r--\n";
    set_acl_from_text(&source, acl_text, acl_sys::ACL_TYPE_ACCESS);

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().acls(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let copied = acl_to_text(&destination, acl_sys::ACL_TYPE_ACCESS).expect("dest acl");
    assert!(copied.contains("user::rw-"));
}

#[test]
fn execute_copies_directory_tree() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"tree").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    assert_eq!(
        fs::read(dest_root.join("nested").join("file.txt")).expect("read"),
        b"tree"
    );
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.directories_created() >= 1);
}

#[test]
fn execute_skips_rewriting_identical_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"identical").expect("write source");
    fs::write(&destination, b"identical").expect("write destination");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    let source_mtime = FileTime::from_last_modification_time(&source_metadata);
    set_file_mtime(&destination, source_mtime).expect("align destination mtime");

    let mut dest_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    dest_perms.set_readonly(true);
    fs::set_permissions(&destination, dest_perms).expect("set destination readonly");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("copy succeeds without rewriting");

    let final_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    assert!(
        !final_perms.readonly(),
        "destination permissions should match writable source"
    );
    assert_eq!(
        fs::read(&destination).expect("destination contents"),
        b"identical"
    );
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_without_times_rewrites_when_checksum_disabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let original_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&destination, original_mtime).expect("set mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let new_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(new_mtime, original_mtime);
}

#[test]
fn execute_without_times_skips_with_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let preserved_mtime = FileTime::from_unix_time(1_700_100_000, 0);
    set_file_mtime(&destination, preserved_mtime).expect("set mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let final_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(final_mtime, preserved_mtime);
}

#[test]
fn execute_with_size_only_skips_same_size_different_content() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"abc").expect("write source");
    fs::write(&dest_path, b"xyz").expect("write destination");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"xyz");
}

#[test]
fn execute_with_ignore_existing_skips_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"original").expect("write destination");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"original");
}

#[test]
fn execute_with_ignore_missing_args_skips_absent_sources() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("missing.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("output.txt");
    fs::write(&destination, b"existing").expect("write destination");

    let operands = vec![
        missing.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_missing_args(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn execute_skips_files_smaller_than_min_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().min_file_size(Some(10)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_skips_files_larger_than_max_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, vec![0u8; 4096]).expect("write large source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_copies_files_matching_size_boundaries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("boundary.bin");
    let destination = temp.path().join("dest.bin");

    let payload = vec![0xAA; 2048];
    fs::write(&source, &payload).expect("write boundary source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(2048))
                .max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 2048);
    assert_eq!(fs::read(&destination).expect("read destination"), payload);
}

#[test]
fn execute_with_update_skips_newer_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source_path, older, older).expect("set source times");
    set_file_times(&dest_path, newer, newer).expect("set dest times");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"existing");
}

#[test]
fn execute_delta_copy_reuses_existing_blocks() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.bin");
    let dest_path = target_root.join("file.bin");

    let mut prefix = vec![b'A'; 700];
    let mut suffix = vec![b'B'; 700];
    let mut replacement = vec![b'C'; 700];

    let mut initial = Vec::new();
    initial.append(&mut prefix.clone());
    initial.append(&mut suffix);
    fs::write(&dest_path, &initial).expect("write initial destination");

    let mut updated = Vec::new();
    updated.append(&mut prefix);
    updated.append(&mut replacement);
    fs::write(&source_path, &updated).expect("write updated source");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
}

#[test]
fn execute_with_report_dry_run_records_file_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"dry-run").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.clone().into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);
    assert_eq!(record.relative_path(), Path::new("source.txt"));
    assert_eq!(record.bytes_transferred(), 7);
}

#[test]
fn execute_with_report_dry_run_records_directory_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("tree");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"data").expect("write nested file");
    let destination = temp.path().join("target");

    let operands = vec![
        source_dir.clone().into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::DirectoryCreated
            && record.relative_path() == Path::new("tree")
    }));
}

#[test]
fn execute_with_report_dry_run_skips_records_for_filtered_small_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    fs::write(&source, b"abc").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.clone().into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .collect_events(true)
        .min_file_size(Some(10));
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    assert!(report.records().is_empty());
    assert_eq!(report.summary().files_copied(), 0);
    assert_eq!(report.summary().regular_files_total(), 1);
    assert_eq!(report.summary().bytes_copied(), 0);
}

#[cfg(unix)]
#[test]
fn execute_copies_symbolic_link() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target").expect("write target");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create link");
    let dest_link = temp.path().join("dest-link");

    let operands = vec![link.into_os_string(), dest_link.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    let copied = fs::read_link(dest_link).expect("read copied link");
    assert_eq!(copied, target);
    assert_eq!(summary.symlinks_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_links_materialises_symlink_to_file() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"payload").expect("write target");

    let link = temp.path().join("link-file");
    symlink(&target, &link).expect("create link");
    let dest = temp.path().join("dest-file");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(&dest).expect("read dest"), b"payload");
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_links_materialises_symlink_to_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("target-dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

    let link = temp.path().join("link-dir");
    symlink(&target_dir, &link).expect("create dir link");
    let dest_dir = temp.path().join("dest-dir");

    let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    let inner = dest_dir.join("inner.txt");
    assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
}

#[cfg(unix)]
#[test]
fn execute_with_copy_dirlinks_follows_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("referenced-dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

    let link = temp.path().join("dir-link");
    symlink(&target_dir, &link).expect("create dir link");
    let dest_dir = temp.path().join("dest-dir");

    let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_dirlinks(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    let inner = dest_dir.join("inner.txt");
    assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
}

#[cfg(unix)]
#[test]
fn execute_with_copy_dirlinks_preserves_file_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"payload").expect("write target");

    let link = temp.path().join("file-link");
    symlink(&target_file, &link).expect("create file link");
    let dest = temp.path().join("dest-link");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_dirlinks(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let copied = fs::read_link(&dest).expect("read link");
    assert_eq!(copied, target_file);
}

#[cfg(unix)]
#[test]
fn execute_with_safe_links_allows_relative_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let nested = source_dir.join("nested");
    fs::create_dir(&nested).expect("create nested");
    let target_file = nested.join("file.txt");
    fs::write(&target_file, b"payload").expect("write target");

    let link_path = source_dir.join("link");
    symlink(Path::new("nested/file.txt"), &link_path).expect("create symlink");
    let destination_link = dest_dir.join("link");

    let operands = vec![
        link_path.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().safe_links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let copied = fs::read_link(&destination_link).expect("read link");
    assert_eq!(copied, Path::new("nested/file.txt"));
}

#[cfg(unix)]
#[test]
fn execute_with_safe_links_skips_unsafe_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let link_path = source_dir.join("escape");
    symlink(Path::new("../../outside"), &link_path).expect("create symlink");
    let destination_link = dest_dir.join("escape");

    let operands = vec![
        link_path.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    assert!(!destination_link.exists());
    let summary = report.summary();
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.symlinks_total(), 1);

    assert!(
        report
            .records()
            .iter()
            .any(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
    );
}

#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_file_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"payload").expect("write outside file");

    let link_path = source_dir.join("escape");
    symlink(&outside_file, &link_path).expect("create symlink");
    let destination_path = dest_dir.join("escape");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read materialised file"),
        b"payload"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_directory_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_dir = temp.path().join("outside-dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    let outside_file = outside_dir.join("file.txt");
    fs::write(&outside_file, b"external").expect("write outside file");

    let link_path = source_dir.join("dirlink");
    symlink(&outside_dir, &link_path).expect("create dir symlink");
    let destination_path = dest_dir.join("dirlink");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
    let copied_file = destination_path.join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"external"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_with_keep_dirlinks_allows_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.clone().into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().keep_dirlinks(true),
        )
        .expect("copy succeeds");

    let copied_file = actual_destination.join("src-dir").join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"payload"
    );
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_keep_dirlinks_rejects_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default());

    let error = result.expect_err("keep-dirlinks disabled should reject destination symlink");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(
            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
        )
    ));
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(!actual_destination.join("src-dir").join("file.txt").exists());
}

#[test]
fn reference_compare_destination_skips_matching_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &reference_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!destination_file.exists());
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn reference_copy_destination_reuses_reference_payload() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_000_500, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &reference_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

#[test]
fn reference_link_destination_degrades_to_copy_on_cross_device_error() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            &reference_dir,
        )]);

    let summary = super::with_hard_link_override(
        |_, _| Err(io::Error::from_raw_os_error(super::CROSS_DEVICE_ERROR_CODE)),
        || {
            plan.execute_with_options(LocalCopyExecution::Apply, options)
                .expect("execution succeeds")
        },
    );

    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn execute_does_not_preserve_metadata_by_default() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");
    fs::write(&destination, b"metadata").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan.execute().expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_ne!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(dest_atime, atime);
    assert_ne!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_metadata_when_requested() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");
    fs::write(&destination, b"metadata").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true).times(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_chmod_modifiers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"payload").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o666)).expect("set perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let modifiers = ChmodModifiers::parse("Fgo-w").expect("chmod parses");
    let options = LocalCopyOptions::default().with_chmod(Some(modifiers));
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_ownership_when_requested() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");

    let owner = 23_456;
    let group = 65_432;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(owner)),
        Some(unix_ids::gid(group)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true).group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), owner);
    assert_eq!(metadata.gid(), group);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_copies_fifo() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o640).expect("mkfifo");

    let atime = FileTime::from_unix_time(1_700_050_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_060_000, 456_000_000);
    set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
    fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o640))
        .expect("set fifo permissions");

    let source_fifo_path = source_fifo.clone();
    let destination_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        destination_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
    assert_eq!(src_metadata.permissions().mode() & 0o777, 0o640);
    let src_atime = FileTime::from_last_access_time(&src_metadata);
    let src_mtime = FileTime::from_last_modification_time(&src_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .times(true)
                .specials(true),
        )
        .expect("fifo copy succeeds");

    let dest_metadata = fs::symlink_metadata(&destination_fifo).expect("dest metadata");
    assert!(dest_metadata.file_type().is_fifo());
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(unix)]
#[test]
fn execute_copies_fifo_within_directory() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("dir");
    fs::create_dir_all(&nested).expect("create nested");

    let source_fifo = nested.join("pipe");
    mkfifo_for_tests(&source_fifo, 0o620).expect("mkfifo");

    let atime = FileTime::from_unix_time(1_700_070_000, 111_000_000);
    let mtime = FileTime::from_unix_time(1_700_080_000, 222_000_000);
    set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
    fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o620))
        .expect("set fifo permissions");

    let source_fifo_path = source_fifo.clone();
    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
    assert_eq!(src_metadata.permissions().mode() & 0o777, 0o620);
    let src_atime = FileTime::from_last_access_time(&src_metadata);
    let src_mtime = FileTime::from_last_modification_time(&src_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .times(true)
                .specials(true),
        )
        .expect("fifo copy succeeds");

    let dest_fifo = dest_root.join("dir").join("pipe");
    let metadata = fs::symlink_metadata(&dest_fifo).expect("dest fifo metadata");
    assert!(metadata.file_type().is_fifo());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o620);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(unix)]
#[test]
fn execute_without_specials_skips_fifo() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        destination_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds without specials");

    assert_eq!(summary.fifos_created(), 0);
    assert!(fs::symlink_metadata(&destination_fifo).is_err());
}

#[cfg(unix)]
#[test]
fn execute_without_specials_records_skip_event() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("skip.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy executes");

    assert!(fs::symlink_metadata(&destination).is_err());
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedNonRegular
            && record.relative_path() == Path::new("skip.pipe")
    }));
}

#[test]
fn execute_with_one_file_system_skips_mount_points() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_dir = source_root.join("mount");
    let mount_file = mount_dir.join("inside.txt");
    let data_dir = source_root.join("data");
    let data_file = data_dir.join("file.txt");
    fs::create_dir_all(&mount_dir).expect("create mount dir");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::write(&mount_file, b"other fs").expect("write mount file");
    fs::write(&data_file, b"same fs").expect("write data file");

    let destination = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|component| component.as_os_str() == std::ffi::OsStr::new("mount"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    assert!(destination.join("data").join("file.txt").exists());
    assert!(!destination.join("mount").exists());
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMountPoint
            && record.relative_path().to_string_lossy().contains("mount")
    }));
}

#[test]
fn execute_without_one_file_system_crosses_mount_points() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_dir = source_root.join("mount");
    let mount_file = mount_dir.join("inside.txt");
    fs::create_dir_all(&mount_dir).expect("create mount dir");
    fs::write(&mount_file, b"other fs").expect("write mount file");

    let destination = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|component| component.as_os_str() == std::ffi::OsStr::new("mount"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().collect_events(true),
            )
        },
    )
    .expect("copy executes");

    assert!(destination.join("mount").join("inside.txt").exists());
    assert!(
        report
            .records()
            .iter()
            .all(|record| { record.action() != &LocalCopyAction::SkippedMountPoint })
    );
}

#[test]
fn execute_with_trailing_separator_copies_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"contents").expect("write file");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");
    assert!(dest_root.join("nested").exists());
    assert!(!dest_root.join("source").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_into_child_directory_succeeds_without_recursing() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("dir");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(nested_dir.join("child.txt"), b"child").expect("write nested");

    let dest_root = source_root.join("child");
    let operands = vec![
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy into child succeeds");

    assert_eq!(
        fs::read(dest_root.join("root.txt")).expect("read root copy"),
        b"root"
    );
    assert_eq!(
        fs::read(dest_root.join("dir").join("child.txt")).expect("read nested copy"),
        b"child"
    );
    assert!(
        !dest_root.join("child").exists(),
        "destination recursion detected at {}",
        dest_root.join("child").display()
    );
    assert!(summary.files_copied() >= 2);
}

#[test]
fn execute_with_delete_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_after_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_after(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_delay_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_before_removes_conflicting_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("file"), b"fresh").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("file")).expect("create conflicting directory");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_before(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with delete-before");

    let target = dest_root.join("file");
    assert_eq!(fs::read(&target).expect("read copied file"), b"fresh");
    assert!(target.is_file());
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.items_deleted() >= 1);
}

#[test]
fn execute_with_max_delete_limit_enforced() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create destination root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra-1.txt"), b"extra").expect("write extra 1");
    fs::write(dest_root.join("extra-2.txt"), b"extra").expect("write extra 2");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(1));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("max-delete should stop deletions");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => assert_eq!(*skipped, 1),
        other => panic!("unexpected error kind: {other:?}"),
    }

    let remaining = [
        dest_root.join("extra-1.txt").exists(),
        dest_root.join("extra-2.txt").exists(),
    ];
    assert!(remaining.iter().copied().any(|exists| exists));
    assert!(remaining.iter().copied().any(|exists| !exists));
}

#[test]
fn execute_with_max_delete_limit_in_dry_run_reports_error() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("obsolete.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(0));

    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should stop deletions when limit is zero");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => assert_eq!(*skipped, 1),
        other => panic!("unexpected error kind: {other:?}"),
    }

    assert!(dest_root.join("obsolete.txt").exists());
}

#[test]
fn execute_with_delete_respects_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"stale"
    );
    assert!(dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_dry_run_leaves_destination_absent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"preview").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with(LocalCopyExecution::DryRun)
        .expect("dry-run succeeds");

    assert!(!destination.exists());
}

#[test]
fn execute_without_implied_dirs_requires_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("missing parent should error");

    match error.kind() {
        LocalCopyErrorKind::Io { action, path, .. } => {
            assert_eq!(*action, "create parent directory");
            assert_eq!(path, destination.parent().expect("parent"));
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(!destination.exists());
}

#[test]
fn execute_dry_run_without_implied_dirs_requires_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should error");

    match error.kind() {
        LocalCopyErrorKind::Io { action, path, .. } => {
            assert_eq!(*action, "create parent directory");
            assert_eq!(path, destination.parent().expect("parent"));
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(!destination.exists());
}

#[test]
fn execute_with_implied_dirs_creates_missing_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"data");
}

#[test]
fn execute_with_mkpath_creates_missing_parents_without_implied_dirs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"data");
}

#[test]
fn execute_with_dry_run_detects_directory_conflict() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"data").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let conflict_dir = dest_root.join("source.txt");
    fs::create_dir_all(&conflict_dir).expect("create conflicting directory");

    let operands = vec![source.into_os_string(), dest_root.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with(LocalCopyExecution::DryRun)
        .expect_err("dry-run should detect conflict");

    match error.into_kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(reason, LocalCopyArgumentError::ReplaceDirectoryWithFile);
        }
        other => panic!("unexpected error kind: {:?}", other),
    }
}

#[cfg(unix)]
#[test]
fn execute_preserves_hard_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_hard_links_materialises_independent_files() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_ne!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 1);
    assert_eq!(metadata_b.nlink(), 1);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn execute_with_delay_updates_preserves_hard_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .hard_links(true)
        .partial(true)
        .delay_updates(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert!(summary.hard_links_created() >= 1);

    for entry in fs::read_dir(&dest_root).expect("read dest") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with(".rsync-tmp-") && !name.starts_with(".rsync-partial-"),
            "unexpected temporary file left behind: {}",
            name
        );
    }
}

#[cfg(unix)]
#[test]
fn execute_with_link_dest_uses_reference_inode() {
    use filetime::FileTime;
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let source_file = source_dir.join("data.txt");
    fs::write(&source_file, b"payload").expect("write source");

    let baseline_dir = temp.path().join("baseline");
    fs::create_dir_all(&baseline_dir).expect("create baseline dir");
    let baseline_file = baseline_dir.join("data.txt");
    fs::write(&baseline_file, b"payload").expect("write baseline");

    let source_metadata = fs::metadata(&source_file).expect("source metadata");
    let source_mtime = source_metadata.modified().expect("source modified time");
    let timestamp = FileTime::from_system_time(source_mtime);
    filetime::set_file_times(&baseline_file, timestamp, timestamp)
        .expect("synchronise baseline timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create destination dir");

    let operands = vec![
        source_file.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([baseline_dir.clone()]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_file = dest_dir.join("data.txt");
    let dest_metadata = fs::metadata(&dest_file).expect("dest metadata");
    let baseline_metadata = fs::metadata(&baseline_file).expect("baseline metadata");

    assert_eq!(dest_metadata.ino(), baseline_metadata.ino());
    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_with_sparse_enabled_creates_holes() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("sparse.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0xAA]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(2 * 1024 * 1024))
        .expect("seek to create hole");
    source_file.write_all(&[0xBB]).expect("write trailing byte");
    source_file.set_len(4 * 1024 * 1024).expect("extend source");

    let dense_dest = temp.path().join("dense.bin");
    let sparse_dest = temp.path().join("sparse-copy.bin");

    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    let plan_sparse = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan sparse");
    plan_sparse
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true),
        )
        .expect("sparse copy succeeds");

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

#[cfg(unix)]
#[test]
fn execute_without_inplace_replaces_destination_file() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"updated").expect("write source");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");
    fs::write(&destination, b"original").expect("write destination");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");
    assert_eq!(summary.files_copied(), 1);

    let updated_metadata = fs::metadata(&destination).expect("destination metadata");
    assert_ne!(updated_metadata.ino(), original_inode);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"updated"
    );

    let mut entries = fs::read_dir(&dest_dir).expect("list dest dir");
    assert!(entries.all(|entry| {
        let name = entry.expect("dir entry").file_name();
        !name.to_string_lossy().starts_with(".rsync-tmp-")
    }));
}

#[cfg(unix)]
#[test]
fn execute_inplace_succeeds_with_read_only_directory() {
    use rustix::fs::{Mode, chmod};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"replacement").expect("write source");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");
    fs::write(&destination, b"original").expect("write destination");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644))
        .expect("make destination writable");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let readonly = Mode::from_bits_truncate(0o555);
    chmod(&dest_dir, readonly).expect("restrict directory permissions");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("in-place copy succeeds");

    let contents = fs::read(&destination).expect("read destination");
    assert_eq!(contents, b"replacement");
    assert_eq!(summary.files_copied(), 1);

    let updated_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();
    assert_eq!(updated_inode, original_inode);

    let restore = Mode::from_bits_truncate(0o755);
    chmod(&dest_dir, restore).expect("restore directory permissions");
}

#[test]
fn execute_with_bandwidth_limit_records_sleep() {
    let mut recorder = rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, vec![0xAA; 4 * 1024]).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(1024).unwrap()));
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest").len(), 4 * 1024);

    let recorded = recorder.take();
    assert!(
        !recorded.is_empty(),
        "expected bandwidth limiter to schedule sleeps"
    );
    let total = recorded
        .into_iter()
        .fold(Duration::ZERO, |acc, duration| acc + duration);
    let expected = Duration::from_secs(4);
    let diff = total.abs_diff(expected);
    assert!(
        diff <= Duration::from_millis(50),
        "expected sleep duration near {:?}, got {:?}",
        expected,
        total
    );
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_append_appends_missing_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"abcdef").expect("write source");
    fs::write(&destination, b"abc").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("append succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
    assert_eq!(summary.bytes_copied(), 3);
    assert_eq!(summary.matched_bytes(), 3);
}

#[test]
fn execute_with_append_verify_rewrites_on_mismatch() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"abcdef").expect("write source");
    fs::write(&destination, b"abx").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("append verify succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
    assert_eq!(summary.bytes_copied(), 6);
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn bandwidth_limiter_limits_chunk_size_for_slow_rates() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    assert_eq!(limiter.recommended_read_size(COPY_BUFFER_SIZE), 512);
    assert_eq!(limiter.recommended_read_size(256), 256);
}

#[test]
fn bandwidth_limiter_preserves_buffer_for_fast_rates() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(8 * 1024 * 1024).unwrap());
    assert_eq!(
        limiter.recommended_read_size(COPY_BUFFER_SIZE),
        COPY_BUFFER_SIZE
    );
}

#[test]
fn execute_without_bandwidth_limit_does_not_sleep() {
    let mut recorder = rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no limit").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), b"no limit");
    assert!(
        recorder.take().is_empty(),
        "unexpected sleep durations recorded"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_compression_records_compressed_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    let content = vec![b'A'; 16 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(summary.compression_used());
    let compressed = summary.compressed_bytes();
    assert!(compressed > 0);
    assert!(compressed <= summary.bytes_copied());
    assert_eq!(summary.bytes_sent(), summary.bytes_received());
    assert_eq!(summary.bytes_sent(), compressed);
}

#[test]
fn execute_records_transmitted_bytes_for_uncompressed_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let payload = b"payload";
    fs::write(&source, payload).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), payload);
    let expected = payload.len() as u64;
    assert_eq!(summary.bytes_copied(), expected);
    assert_eq!(summary.bytes_sent(), expected);
    assert_eq!(summary.bytes_received(), expected);
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn execute_with_compression_limits_post_compress_bandwidth() {
    let mut recorder = rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    let mut content = Vec::new();
    for _ in 0..4096 {
        content.extend_from_slice(b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \n");
    }
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let limit = NonZeroU64::new(2 * 1024).expect("limit");
    let options = LocalCopyOptions::default()
        .compress(true)
        .bandwidth_limit(Some(limit));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(summary.compression_used());

    let compressed = summary.compressed_bytes();
    assert!(compressed > 0);
    let transferred = summary.bytes_copied();

    let sleeps = recorder.take();
    assert!(
        !sleeps.is_empty(),
        "bandwidth limiter did not record sleeps"
    );
    let total_sleep_secs: f64 = sleeps.iter().map(|duration| duration.as_secs_f64()).sum();

    let expected_compressed = compressed as f64 / limit.get() as f64;
    let expected_uncompressed = transferred as f64 / limit.get() as f64;

    let tolerance = expected_compressed * 0.2 + 0.2;
    assert!(
        (total_sleep_secs - expected_compressed).abs() <= tolerance,
        "sleep {:?}s deviates too far from compressed expectation {:?}s",
        total_sleep_secs,
        expected_compressed,
    );
    assert!(
        (total_sleep_secs - expected_compressed).abs()
            < (total_sleep_secs - expected_uncompressed).abs(),
        "sleep {:?}s should align with compressed bytes ({:?}s) rather than uncompressed ({:?}s)",
        total_sleep_secs,
        expected_compressed,
        expected_uncompressed,
    );
}

#[test]
fn execute_respects_exclude_filter() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default().filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_prunes_empty_directories_when_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest_without_prune = temp.path().join("dest_without_prune");
    let dest_with_prune = temp.path().join("dest_with_prune");

    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(source.join("empty")).expect("create empty dir");
    fs::write(source.join("keep.txt"), b"payload").expect("write keep");
    fs::write(source.join("empty").join("skip.tmp"), b"skip").expect("write skip");

    fs::create_dir_all(&dest_without_prune).expect("create dest");
    fs::create_dir_all(&dest_with_prune).expect("create dest");

    let operands_without = vec![
        source.clone().into_os_string(),
        dest_without_prune.clone().into_os_string(),
    ];
    let plan_without = LocalCopyPlan::from_operands(&operands_without).expect("plan");
    let filters_without = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options_without = LocalCopyOptions::default().filters(Some(filters_without));
    let summary_without = plan_without
        .execute_with_options(LocalCopyExecution::Apply, options_without)
        .expect("copy succeeds");

    let operands_with = vec![
        source.into_os_string(),
        dest_with_prune.clone().into_os_string(),
    ];
    let plan_with = LocalCopyPlan::from_operands(&operands_with).expect("plan");
    let filters_with = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options_with = LocalCopyOptions::default()
        .filters(Some(filters_with))
        .prune_empty_dirs(true);
    let summary_with = plan_with
        .execute_with_options(LocalCopyExecution::Apply, options_with)
        .expect("copy succeeds");

    let target_without = dest_without_prune.join("source");
    let target_with = dest_with_prune.join("source");

    assert!(target_without.join("keep.txt").exists());
    assert!(target_with.join("keep.txt").exists());
    assert!(target_without.join("empty").is_dir());
    assert!(!target_with.join("empty").exists());
    assert!(summary_with.directories_created() < summary_without.directories_created());
}

#[test]
fn execute_prunes_empty_directories_with_size_filters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    let nested = source_root.join("nested");

    fs::create_dir_all(&nested).expect("create nested source");
    fs::create_dir_all(&destination_root).expect("create destination root");
    fs::write(nested.join("tiny.bin"), b"x").expect("write small file");

    let operands = vec![
        source_root.clone().into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(10))
        .prune_empty_dirs(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.exists());
    assert!(target_root.join("nested").is_dir());
    assert!(!target_root.join("nested").join("tiny.bin").exists());
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn execute_respects_include_filter_override() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.tmp"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([
        rsync_filters::FilterRule::exclude("*.tmp"),
        rsync_filters::FilterRule::include("keep.tmp"),
    ])
    .expect("compile filters");
    let options = LocalCopyOptions::default().filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.tmp").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_skips_directories_with_exclude_if_present_marker() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&destination_root).expect("create dest root");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    let marker_dir = source_root.join("skip");
    fs::create_dir_all(&marker_dir).expect("create marker dir");
    fs::write(marker_dir.join(".rsyncignore"), b"marker").expect("write marker");
    fs::write(marker_dir.join("data.txt"), b"ignored").expect("write data");

    let operands = vec![
        source_root.clone().into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(".rsyncignore"),
    )])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip").exists());
}

#[test]
fn dir_merge_exclude_if_present_from_filter_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&destination_root).expect("create dest");

    fs::write(
        source_root.join(".rsync-filter"),
        b"exclude-if-present=.git\n",
    )
    .expect("write filter");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let project = source_root.join("project");
    fs::create_dir_all(&project).expect("create project");
    fs::write(project.join(".git"), b"marker").expect("write marker");
    fs::write(project.join("data.txt"), b"ignored").expect("write data");

    let operands = vec![
        source_root.clone().into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        PathBuf::from(".rsync-filter"),
        DirMergeOptions::default(),
    ))])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("project").exists());
}

#[test]
fn filter_program_clear_discards_previous_rules() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&destination_root).expect("create dest");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"tmp").expect("write tmp");
    fs::write(source_root.join("skip.bak"), b"bak").expect("write bak");

    let operands = vec![
        source_root.clone().into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Clear,
        FilterProgramEntry::Rule(FilterRule::exclude("*.bak")),
    ])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(
        target_root.join("skip.tmp").exists(),
        "list-clearing rule should discard earlier excludes"
    );
    assert!(!target_root.join("skip.bak").exists());
}

#[test]
fn dir_merge_clear_keyword_discards_previous_rules() {
    let temp = tempdir().expect("tempdir");
    let filter = temp.path().join("filter.rules");
    fs::write(&filter, b"exclude-if-present=.git\nclear\n- skip\n").expect("write filter");

    let mut visited = Vec::new();
    let options = DirMergeOptions::default();
    let entries =
        load_dir_merge_rules_recursive(&filter, &options, &mut visited).expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.rules.iter().any(|rule| {
        rule.pattern() == "skip" && matches!(rule.action(), rsync_filters::FilterAction::Exclude)
    }));
    assert!(entries.exclude_if_present.is_empty());
}

#[test]
fn dir_merge_clear_keyword_discards_rules_in_whitespace_mode() {
    let temp = tempdir().expect("tempdir");
    let filter = temp.path().join("filter.rules");
    fs::write(&filter, b"exclude-if-present=.git clear -skip").expect("write filter");

    let mut visited = Vec::new();
    let options = DirMergeOptions::default()
        .use_whitespace()
        .allow_list_clearing(true);
    let entries =
        load_dir_merge_rules_recursive(&filter, &options, &mut visited).expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.exclude_if_present.is_empty());
}

#[test]
fn delete_respects_exclude_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("extra.txt").exists());
    let skip_path = target_root.join("skip.tmp");
    assert!(skip_path.exists());
    assert_eq!(fs::read(skip_path).expect("read skip"), b"dest skip");
    assert!(summary.files_copied() >= 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_excluded_entries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_matching_source_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip source").expect("write skip source");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn backup_creation_uses_default_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"updated").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"original").expect("write dest");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"original");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"updated"
    );
}

#[test]
fn backup_creation_respects_custom_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"replacement").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"baseline").expect("write dest");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt.bak");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"baseline");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"replacement"
    );
}

#[test]
fn backup_creation_uses_relative_backup_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("dir").join("file.txt");
    fs::create_dir_all(source_file.parent().unwrap()).expect("create nested source");
    fs::write(&source_file, b"new contents").expect("write source");

    let dest_root = dest.join("source");
    let existing_parent = dest_root.join("dir");
    fs::create_dir_all(&existing_parent).expect("create dest root");
    let existing = existing_parent.join("file.txt");
    fs::write(&existing, b"old contents").expect("write dest");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_backup_directory(Some(PathBuf::from("backups")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest
        .join("backups")
        .join("source")
        .join("dir")
        .join("file.txt~");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old contents");
    assert_eq!(
        fs::read(dest_root.join("dir").join("file.txt")).expect("read dest"),
        b"new contents"
    );
}

#[test]
fn backup_creation_uses_absolute_backup_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    let backup_root = temp.path().join("backups");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"replacement").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"retained").expect("write dest");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_root.as_path().to_path_buf()))
        .with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = backup_root.join("source").join("file.txt.bak");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"retained");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"replacement"
    );
}

#[test]
fn delete_respects_protect_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::protect("keep.txt")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 0);
}

#[test]
fn delete_risk_rule_overrides_protection() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([
        rsync_filters::FilterRule::protect("keep.txt"),
        rsync_filters::FilterRule::risk("keep.txt"),
    ])
    .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(!target_root.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn destination_write_guard_uses_custom_partial_directory() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let partial_dir = Path::new(".custom-partial");

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), true, Some(partial_dir), None)
            .expect("guard");
    let temp_path = guard.temp_path.clone();
    file.write_all(b"partial payload").expect("write partial");
    drop(file);

    drop(guard);

    let expected_base = destination_dir.join(partial_dir);
    assert!(temp_path.starts_with(&expected_base));
    assert!(temp_path.exists());
    assert!(!destination.exists());
}

#[test]
fn destination_write_guard_commit_moves_from_partial_directory() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let partial_dir = temp.path().join("partials");

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(partial_dir.as_path()),
        None,
    )
    .expect("guard");
    let temp_path = guard.temp_path.clone();
    file.write_all(b"committed payload").expect("write payload");
    drop(file);

    guard.commit().expect("commit succeeds");

    assert!(!temp_path.exists());
    let committed = fs::read(&destination).expect("read committed file");
    assert_eq!(committed, b"committed payload");
}

#[test]
fn destination_write_guard_uses_custom_temp_directory_for_non_partial() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let temp_dir = temp.path().join("tmp-area");
    fs::create_dir_all(&temp_dir).expect("temp dir");

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), false, None, Some(temp_dir.as_path()))
            .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"temporary payload").expect("write temp");
    drop(file);

    guard.commit().expect("commit");

    assert!(staging_path.starts_with(&temp_dir));
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"temporary payload"
    );
    assert!(!staging_path.exists());
}
