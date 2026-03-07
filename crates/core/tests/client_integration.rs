use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use core::client::{
    ClientConfig, ClientEventKind, ClientProgressObserver, ClientProgressUpdate, FilterRuleSpec,
    run_client, run_client_with_observer,
};
use tempfile::tempdir;

/// Sets the creation time (birth time) of a file via `setattrlist(2)`.
///
/// Only available on macOS where creation time is settable.
#[cfg(target_os = "macos")]
fn set_birthtime(path: &Path, secs: i64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct AttrBuf {
        timespec: libc::timespec,
    }

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("valid path");

    let mut attrlist: libc::attrlist = unsafe { std::mem::zeroed() };
    attrlist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrlist.commonattr = libc::ATTR_CMN_CRTIME;

    let buf = AttrBuf {
        timespec: libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
    };

    // SAFETY: c_path is a valid NUL-terminated C string, attrlist is zeroed
    // then configured with valid bitmap values, and buf has the exact layout
    // expected by setattrlist(2).
    let ret = unsafe {
        libc::setattrlist(
            c_path.as_ptr(),
            &attrlist as *const _ as *mut _,
            &buf as *const _ as *mut libc::c_void,
            std::mem::size_of::<AttrBuf>(),
            0,
        )
    };
    assert_eq!(
        ret,
        0,
        "setattrlist failed: {}",
        std::io::Error::last_os_error()
    );
}

/// Reads the creation time (birth time) of a file as seconds since the Unix epoch.
///
/// Only available on macOS where creation time is reliably reported.
#[cfg(target_os = "macos")]
fn get_birthtime_secs(path: &Path) -> i64 {
    use std::os::darwin::fs::MetadataExt;

    let meta = fs::metadata(path).expect("read metadata");
    meta.st_birthtime()
}

fn touch(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
}

#[test]
fn run_client_copies_with_delete_and_filters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    touch(&source_root.join("keep.txt"), b"keep");
    touch(&source_root.join("nested/data.bin"), b"payload");
    touch(&source_root.join("remove.tmp"), b"temporary");

    #[cfg(unix)]
    std::os::unix::fs::symlink("keep.txt", source_root.join("keep-link")).expect("symlink");

    touch(&dest_root.join("stale.txt"), b"obsolete");
    touch(&dest_root.join("remove.tmp"), b"old temporary");
    touch(&dest_root.join("protected.txt"), b"protected");

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .mkpath(true)
        .delete_before(true)
        .delete_excluded(true)
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .add_filter_rule(FilterRuleSpec::protect("protected.txt"))
        .permissions(true)
        .links(true)
        .times(true)
        .progress(true)
        .stats(true)
        .build();

    let summary = run_client(config).expect("run client");

    assert_eq!(fs::read(dest_root.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(
        fs::read(dest_root.join("nested/data.bin")).unwrap(),
        b"payload"
    );
    #[cfg(unix)]
    {
        let target = fs::read_link(dest_root.join("keep-link")).expect("symlink created");
        assert_eq!(target, PathBuf::from("keep.txt"));
    }
    assert!(
        !dest_root.join("remove.tmp").exists(),
        "excluded files deleted"
    );
    assert!(
        dest_root.join("protected.txt").exists(),
        "protected entries are preserved"
    );
    assert!(
        !dest_root.join("stale.txt").exists(),
        "stale entries removed during delete-before"
    );

    assert!(summary.files_copied() >= 2);
    assert!(summary.items_deleted() >= 1);
    assert!(summary.bytes_copied() > 0);
}

#[derive(Default)]
struct RecordingObserver {
    updates: Vec<(PathBuf, ClientEventKind, bool, Option<u64>, u64)>,
}

impl ClientProgressObserver for RecordingObserver {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        self.updates.push((
            update.event().relative_path().to_path_buf(),
            update.event().kind().clone(),
            update.is_final(),
            update.total_bytes(),
            update.overall_transferred(),
        ));
    }
}

#[test]
fn progress_observer_reports_transfers() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let dest_root = temp.path().join("dst");

    touch(&source_root.join("first.bin"), b"1234567890");
    touch(&source_root.join("nested/second.bin"), b"abcdefghij");

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.into_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .build();

    let mut observer = RecordingObserver::default();
    let summary = run_client_with_observer(config, Some(&mut observer)).expect("run client");

    assert_eq!(summary.files_copied(), 2);
    let data_updates: Vec<_> = observer
        .updates
        .iter()
        .filter(|(_, kind, ..)| matches!(kind, ClientEventKind::DataCopied))
        .collect();
    assert!(data_updates.len() >= 2, "expected data copy updates");

    let mut seen_nested = false;
    let mut last_transferred = 0;
    let mut completions: HashMap<PathBuf, bool> = HashMap::new();
    for (path, _, final_update, total_bytes, transferred) in data_updates {
        let file_name = path
            .file_name()
            .expect("progress events reference concrete files");
        assert!(
            file_name.to_string_lossy().ends_with(".bin"),
            "unexpected event path: {path:?}"
        );
        assert!(total_bytes.is_some(), "byte counts emitted for each file");
        assert!(
            *transferred >= last_transferred,
            "progress increments transferred bytes"
        );
        last_transferred = *transferred;
        if path
            .components()
            .any(|component| component.as_os_str() == "nested")
        {
            seen_nested = true;
        }
        completions
            .entry(path.clone())
            .and_modify(|done| *done |= *final_update)
            .or_insert(*final_update);
    }
    assert!(seen_nested, "progress includes nested entries");
    assert!(
        completions.values().filter(|done| **done).count() >= 2,
        "all files eventually report completion"
    );
}

/// Verifies that `--atimes` (`-U`) preserves access times on destination files
/// and directories during a local transfer.
#[cfg(unix)]
#[test]
fn test_atimes_preservation() {
    use filetime::FileTime;
    use std::time::Duration;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create source files with distinct content sizes so quick-check never skips them.
    touch(&source_root.join("alpha.txt"), b"alpha-content");
    touch(&source_root.join("subdir/beta.txt"), b"beta-content-longer");

    // Set well-known timestamps on source files and the subdirectory.
    // Use second-level precision - upstream rsync protocol transmits times as
    // whole seconds, so sub-second components are not preserved over the wire.
    let src_atime = FileTime::from_unix_time(1_600_000_000, 0);
    let src_mtime = FileTime::from_unix_time(1_650_000_000, 0);

    let src_dir_atime = FileTime::from_unix_time(1_500_000_000, 0);
    let src_dir_mtime = FileTime::from_unix_time(1_550_000_000, 0);

    filetime::set_file_times(source_root.join("alpha.txt"), src_atime, src_mtime)
        .expect("set alpha timestamps");
    filetime::set_file_times(source_root.join("subdir/beta.txt"), src_atime, src_mtime)
        .expect("set beta timestamps");
    filetime::set_file_times(source_root.join("subdir"), src_dir_atime, src_dir_mtime)
        .expect("set subdir timestamps");

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .mkpath(true)
        .atimes(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("atimes transfer succeeds");
    assert!(summary.files_copied() >= 2);

    // Helper: assert that two `FileTime` values are within `tolerance` of each other.
    let assert_time_close = |actual: FileTime, expected: FileTime, label: &str| {
        let diff_secs = (actual.unix_seconds() - expected.unix_seconds()).unsigned_abs();
        let tolerance = Duration::from_secs(2);
        assert!(
            diff_secs <= tolerance.as_secs(),
            "{label}: expected {expected:?}, got {actual:?} (diff {diff_secs}s)"
        );
    };

    // Verify file atimes and mtimes.
    for name in &["alpha.txt", "subdir/beta.txt"] {
        let dest_path = dest_root.join(name);
        let meta = fs::metadata(&dest_path).unwrap_or_else(|e| panic!("metadata for {name}: {e}"));
        let dest_atime = FileTime::from_last_access_time(&meta);
        let dest_mtime = FileTime::from_last_modification_time(&meta);

        assert_time_close(dest_atime, src_atime, &format!("{name} atime"));
        assert_time_close(dest_mtime, src_mtime, &format!("{name} mtime"));
    }

    // Verify subdirectory timestamps.
    let subdir_meta = fs::metadata(dest_root.join("subdir")).expect("subdir metadata");
    let subdir_atime = FileTime::from_last_access_time(&subdir_meta);
    let subdir_mtime = FileTime::from_last_modification_time(&subdir_meta);

    assert_time_close(subdir_atime, src_dir_atime, "subdir atime");
    assert_time_close(subdir_mtime, src_dir_mtime, "subdir mtime");
}

/// Verifies that `crtimes(true)` preserves file creation times end-to-end.
///
/// Sets a known birthtime on source files via `setattrlist(2)`, transfers with
/// crtime preservation enabled, then confirms destination files have the same
/// creation time.
#[cfg(target_os = "macos")]
#[test]
fn test_crtimes_preservation() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("crtime-src");
    let dest_root = temp.path().join("crtime-dst");

    fs::create_dir_all(&source_root).expect("source root");

    touch(&source_root.join("alpha.txt"), b"alpha content");
    touch(&source_root.join("nested/beta.txt"), b"beta content here");

    // Backdate birthtimes to a known epoch well in the past so they are
    // clearly distinguishable from the transfer time.
    let alpha_crtime: i64 = 1_600_000_000; // 2020-09-13
    let beta_crtime: i64 = 1_500_000_000; // 2017-07-14
    set_birthtime(&source_root.join("alpha.txt"), alpha_crtime);
    set_birthtime(&source_root.join("nested/beta.txt"), beta_crtime);

    // Confirm source birthtimes were set correctly before transfer.
    assert_eq!(
        get_birthtime_secs(&source_root.join("alpha.txt")),
        alpha_crtime,
        "source alpha birthtime must match the value we set"
    );
    assert_eq!(
        get_birthtime_secs(&source_root.join("nested/beta.txt")),
        beta_crtime,
        "source beta birthtime must match the value we set"
    );

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .mkpath(true)
        .times(true)
        .crtimes(true)
        .build();

    let summary = run_client(config).expect("run client with crtimes");

    assert!(
        summary.files_copied() >= 2,
        "both files should be transferred"
    );

    // Verify file contents arrived.
    assert_eq!(
        fs::read(dest_root.join("alpha.txt")).unwrap(),
        b"alpha content"
    );
    assert_eq!(
        fs::read(dest_root.join("nested/beta.txt")).unwrap(),
        b"beta content here"
    );

    // Verify creation times were preserved on the destination.
    assert_eq!(
        get_birthtime_secs(&dest_root.join("alpha.txt")),
        alpha_crtime,
        "destination alpha birthtime should match source"
    );
    assert_eq!(
        get_birthtime_secs(&dest_root.join("nested/beta.txt")),
        beta_crtime,
        "destination beta birthtime should match source"
    );
}
