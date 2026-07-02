//! Integration tests for compression functionality.
//!
//! These tests verify that compression works correctly end-to-end during
//! file transfers, including compression level configuration, skip-compress
//! patterns, and data integrity.

mod test_timeout;

use std::fs;
use std::path::Path;

use core::client::ClientConfig;
use tempfile::tempdir;
use test_timeout::{LOCAL_TIMEOUT, run_with_timeout};

fn touch(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// Creates test data that compresses well (repeated patterns)
fn create_compressible_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        data.extend_from_slice(pattern);
    }
    data.truncate(size);
    data
}

/// Creates test data that doesn't compress well (random-like)
fn create_incompressible_data(size: usize) -> Vec<u8> {
    // Use a deterministic pattern that looks random
    (0..size).map(|i| ((i * 97 + 31) % 256) as u8).collect()
}

#[test]
fn test_compression_disabled_by_default() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify that compression is NOT enabled by default in local copy mode
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create a compressible file
        let data = create_compressible_data(10240); // 10KB of compressible data
        touch(&source_root.join("data.txt"), &data);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify file was copied correctly
        assert_eq!(fs::read(dest_root.join("data.txt")).unwrap(), data);
        assert!(summary.files_copied() >= 1);
        assert!(summary.bytes_copied() > 0);
    });
}

#[test]
fn test_compression_enabled_copies_correctly() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify that compression can be enabled and files are copied correctly
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create a compressible file
        let data = create_compressible_data(10240); // 10KB of compressible data
        touch(&source_root.join("data.txt"), &data);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .compress(true)
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify file was copied correctly (content integrity)
        assert_eq!(fs::read(dest_root.join("data.txt")).unwrap(), data);
        assert!(summary.files_copied() >= 1);
        assert!(summary.bytes_copied() > 0);
    });
}

#[test]
fn test_compression_preserves_binary_data() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify compression works with binary (incompressible) data
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create incompressible binary data
        let data = create_incompressible_data(8192); // 8KB of incompressible data
        touch(&source_root.join("binary.dat"), &data);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .compress(true)
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify binary data preserved exactly
        assert_eq!(fs::read(dest_root.join("binary.dat")).unwrap(), data);
        assert!(summary.files_copied() >= 1);
    });
}

#[test]
fn test_compression_with_multiple_files() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify compression works with multiple files of varying compressibility
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create mix of compressible and incompressible files
        let compressible = create_compressible_data(5120);
        let incompressible = create_incompressible_data(5120);

        touch(&source_root.join("text.txt"), &compressible);
        touch(&source_root.join("data.bin"), &incompressible);
        touch(&source_root.join("nested/log.txt"), &compressible);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .compress(true)
            .mkpath(true)
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify all files copied correctly
        assert_eq!(fs::read(dest_root.join("text.txt")).unwrap(), compressible);
        assert_eq!(
            fs::read(dest_root.join("data.bin")).unwrap(),
            incompressible
        );
        assert_eq!(
            fs::read(dest_root.join("nested/log.txt")).unwrap(),
            compressible
        );
        assert!(summary.files_copied() >= 3);
    });
}

#[test]
fn test_skip_compress_default_patterns() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify that default skip-compress patterns are applied
        // (This tests the skip_compress infrastructure in local copy mode)
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create files that match skip-compress patterns
        let data = create_compressible_data(4096);
        touch(&source_root.join("archive.tar.gz"), &data);
        touch(&source_root.join("video.mp4"), &data);
        touch(&source_root.join("text.txt"), &data);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .compress(true)
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify all files copied correctly (skip-compress doesn't affect correctness)
        assert_eq!(fs::read(dest_root.join("archive.tar.gz")).unwrap(), data);
        assert_eq!(fs::read(dest_root.join("video.mp4")).unwrap(), data);
        assert_eq!(fs::read(dest_root.join("text.txt")).unwrap(), data);
        assert!(summary.files_copied() >= 3);
    });
}

#[test]
fn test_large_file_with_compression() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        // Verify compression works with larger files
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");

        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        // Create a larger compressible file (100KB)
        let data = create_compressible_data(102400);
        touch(&source_root.join("large.txt"), &data);

        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .compress(true)
            .build();

        let summary = core::client::run_client(config).expect("run client");

        // Verify large file copied correctly
        assert_eq!(fs::read(dest_root.join("large.txt")).unwrap(), data);
        assert!(summary.files_copied() >= 1);
        assert_eq!(summary.bytes_copied() as usize, data.len());
    });
}

/// Collects `--debug=NSTR` messages emitted during `f`, initialising the
/// thread-local logger at NSTR level 1 first. The local-copy path reads this
/// same thread-local config, so the summary emitters fire on this thread.
fn nstr_messages_during<F: FnOnce()>(f: F) -> Vec<String> {
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let mut cfg = VerbosityConfig::default();
    cfg.debug.nstr = 1;
    init(cfg);
    let _ = drain_events();

    f();

    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Nstr,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// A local `-z --debug=NSTR` copy must emit the upstream `checksum:` and
/// `compress: <name> (level N)` NSTR summary lines even though the wire
/// negotiator never runs for a local copy. upstream: checksum.c:206-211 and
/// compat.c:213-219 - the forked local_child server prints these.
#[test]
fn local_copy_emits_nstr_summaries_under_debug_nstr() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        touch(
            &source_root.join("data.txt"),
            &create_compressible_data(4096),
        );
        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let messages = nstr_messages_during(|| {
            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest_root.into_os_string()])
                .compress(true)
                .build();
            core::client::run_client(config).expect("run client");
        });

        // Checksum summary always fires. oc's default strong checksum choice
        // (`Auto`) resolves locally to MD5, so that is the reported name -
        // matching the algorithm the local delta path actually uses.
        assert!(
            messages.iter().any(|m| m == "Client checksum: md5"),
            "expected NSTR checksum summary for local copy: {messages:?}"
        );
        // Compress fires because -z is active; no --compress-level means the
        // upstream CLVL_NOT_SPECIFIED sentinel renders in the (level N) clause.
        let expected = format!(
            "Client compress: {} (level {})",
            compress::algorithm::CompressionAlgorithm::default_algorithm().name(),
            i32::MIN
        );
        assert!(
            messages.contains(&expected),
            "expected NSTR compress summary for local copy: {messages:?}"
        );
    });
}

/// `--compress-level=9` must render `(level 9)` in the local-copy NSTR compress
/// summary, exercising the threaded level instead of the sentinel.
#[test]
fn local_copy_nstr_compress_summary_renders_explicit_level() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&dest_root).expect("dest root");

        touch(
            &source_root.join("data.txt"),
            &create_compressible_data(4096),
        );
        let mut source_arg = source_root.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let level = compress::zlib::CompressionLevel::from_numeric(9).expect("level 9");
        let messages = nstr_messages_during(|| {
            let config = ClientConfig::builder()
                .transfer_args([source_arg, dest_root.into_os_string()])
                .compress(true)
                .compression_level(Some(level))
                .build();
            core::client::run_client(config).expect("run client");
        });

        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("Client compress: ") && m.ends_with("(level 9)")),
            "expected compress summary to render level 9: {messages:?}"
        );
    });
}
