//! Mocks the io_uring runtime probe so the standard-I/O fallback path can be
//! exercised on hosts that would otherwise satisfy the kernel probe.
//!
//! The probe in [`fast_io::is_io_uring_available`] checks an environment
//! variable (`OC_RSYNC_DISABLE_IOURING`) on every call and reports `false`
//! when it is set to a truthy value. That makes the fallback path reachable
//! on Linux 5.6+ runners in CI, on emulators that lack `io_uring_setup(2)`,
//! and inside containers that block the syscall via seccomp - without
//! requiring a particular kernel.
//!
//! These tests are gated to Linux because the env-var hook is only meaningful
//! when io_uring would otherwise be a candidate. On non-Linux platforms
//! [`fast_io::is_io_uring_available`] already returns `false`, so the
//! fallback path is the only path and there is nothing to mock.

#![cfg(target_os = "linux")]

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::sync::{Mutex, MutexGuard};

use fast_io::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringPolicy, IoUringReaderFactory,
    IoUringWriterFactory, is_io_uring_available, reader_from_path, writer_from_file,
};
use tempfile::tempdir;

const DISABLE_VAR: &str = "OC_RSYNC_DISABLE_IOURING";

/// Serialises tests in this binary so that environment mutations do not
/// interleave. Rust 2024 requires `set_var`/`remove_var` to be wrapped in
/// `unsafe`; this guard is the only place we touch the env in this file.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let previous = env::var_os(key);
        // SAFETY: serialised via ENV_LOCK; no other thread mutates this var.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: ENV_LOCK is still held for the lifetime of `_lock`.
        #[allow(unsafe_code)]
        unsafe {
            match &self.previous {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }
}

/// With the disable env var set to a truthy value, the cached probe must
/// report `false` regardless of kernel version. This is the contract that
/// callers and CI rely on to exercise the standard-I/O writer path.
#[test]
fn disable_env_var_forces_probe_to_report_unavailable() {
    let _g = EnvGuard::set(DISABLE_VAR, OsStr::new("1"));
    assert!(
        !is_io_uring_available(),
        "OC_RSYNC_DISABLE_IOURING=1 must force probe to report unavailable"
    );
}

/// The disable hook must accept the common truthy spellings so operators do
/// not have to memorise a single magic value.
#[test]
fn disable_env_var_accepts_common_truthy_spellings() {
    for value in ["1", "true", "TRUE", "yes", "YES", "on", "On"] {
        let _g = EnvGuard::set(DISABLE_VAR, OsStr::new(value));
        assert!(
            !is_io_uring_available(),
            "OC_RSYNC_DISABLE_IOURING={value} must force probe off"
        );
    }
}

/// Auto-policy writes must still succeed when the probe is forced off; the
/// content must round-trip through the standard buffered writer.
#[test]
fn auto_policy_writer_falls_back_to_std_when_probe_disabled() {
    let _g = EnvGuard::set(DISABLE_VAR, OsStr::new("1"));

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("disabled_probe.bin");
    let payload = b"mocked probe fallback content";

    {
        let file = std::fs::File::create(&path).expect("create file");
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Auto)
            .expect("Auto writer must succeed when probe forced off");
        assert!(
            matches!(writer, IoUringOrStdWriter::Std(_)),
            "writer must be the Std variant when probe is forced off"
        );
        writer.write_all(payload).expect("write_all");
        writer.flush().expect("flush");
    }

    let mut reader =
        reader_from_path(&path, IoUringPolicy::Auto).expect("Auto reader must succeed");
    assert!(
        matches!(reader, IoUringOrStdReader::Std(_)),
        "reader must be the Std variant when probe is forced off"
    );
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end");
    assert_eq!(buf.as_slice(), payload);
}

/// The factory types expose `will_use_io_uring()` so callers can branch up
/// front. When the probe is mocked off they must report `false` even though
/// the kernel may support io_uring natively.
#[test]
fn factory_will_use_io_uring_returns_false_when_probe_disabled() {
    let _g = EnvGuard::set(DISABLE_VAR, OsStr::new("1"));

    let reader_factory = IoUringReaderFactory::default();
    let writer_factory = IoUringWriterFactory::default();

    assert!(!reader_factory.will_use_io_uring());
    assert!(!writer_factory.will_use_io_uring());
}

/// Enabled policy must return an `Unsupported` error when the probe has been
/// mocked off, mirroring the behaviour on a kernel that genuinely lacks
/// io_uring. This is the path CI uses to assert the error mapping.
#[test]
fn enabled_policy_returns_unsupported_when_probe_disabled() {
    let _g = EnvGuard::set(DISABLE_VAR, OsStr::new("1"));

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("enabled_probe_disabled.bin");
    let file = std::fs::File::create(&path).expect("create file");

    let writer_err = writer_from_file(file, 8192, IoUringPolicy::Enabled)
        .expect_err("Enabled policy must error when probe is forced off");
    assert_eq!(writer_err.kind(), std::io::ErrorKind::Unsupported);
    assert!(writer_err.to_string().to_lowercase().contains("io_uring"));

    let reader_err = reader_from_path(&path, IoUringPolicy::Enabled)
        .expect_err("Enabled reader must error when probe is forced off");
    assert_eq!(reader_err.kind(), std::io::ErrorKind::Unsupported);
    assert!(reader_err.to_string().to_lowercase().contains("io_uring"));
}

/// Disabled policy already bypasses the probe, so the env var override must
/// be a no-op for it. The path must continue to produce a working std writer
/// and reader pair with byte-exact round trip.
#[test]
fn disabled_policy_round_trips_regardless_of_env_var() {
    let _g = EnvGuard::set(DISABLE_VAR, OsStr::new("1"));

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("disabled_policy.bin");
    let payload = b"disabled policy still works";

    {
        let file = std::fs::File::create(&path).expect("create");
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).expect("writer");
        assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
        writer.write_all(payload).expect("write");
        writer.flush().expect("flush");
    }

    let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).expect("reader");
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read");
    assert_eq!(buf.as_slice(), payload);
}

/// Unknown / falsy values for the env var must not disable the probe. This
/// guards against accidentally tripping the mock through stray exports.
#[test]
fn falsy_env_values_do_not_disable_probe() {
    // Capture the natural probe result before mutating the env. The cached
    // outcome is what the implementation must keep producing.
    let baseline = is_io_uring_available();

    for value in ["0", "false", "no", "off", "", "maybe", "nope"] {
        let _g = EnvGuard::set(DISABLE_VAR, OsStr::new(value));
        assert_eq!(
            is_io_uring_available(),
            baseline,
            "OC_RSYNC_DISABLE_IOURING={value:?} must not change the probe outcome"
        );
    }
}
