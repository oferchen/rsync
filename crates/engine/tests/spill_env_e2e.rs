//! End-to-end integration test for the env-var driven spill layer.
//!
//! Drives a local-copy transfer over 10 MB of mixed-content data with a low
//! `OC_RSYNC_SPILL_THRESHOLD_BYTES` set, then asserts that the spill layer
//! engaged: at least one spill event was recorded and at least one tempfile
//! landed in the directory pointed at by `OC_RSYNC_SPILL_DIR`.
//!
//! # Why this test is currently `#[ignore]`
//!
//! The env-var wiring this test exercises lives behind dependent tasks
//! (`STN-8` parses the env vars, `STN-9`/`STN-10` propagate them into the
//! engine pipeline that the local-copy executor uses). Until that wiring
//! lands, `LocalCopyPlan::execute` does not consult the env so the spill
//! counter stays at zero regardless of the threshold. The file compiles and
//! the test body runs to completion when un-ignored; only the final
//! assertions block on the missing wiring.
//!
//! # Hermeticity
//!
//! * Source, destination, and the spill scratch directory are all
//!   `tempfile::TempDir` handles. Nothing escapes the test's scratch root.
//! * Env mutations are serialised by a per-process mutex and restored on
//!   drop via the inline `EnvGuard` helper. Failing assertions still run
//!   the guard `Drop`, so a panic cannot leak `OC_RSYNC_SPILL_*` to other
//!   tests.
//! * Pseudo-random payload bytes come from a deterministic xorshift PRNG
//!   seeded with a constant, so the 10 MB corpus is byte-identical across
//!   runs without pulling in `rand`.

#![cfg(unix)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use engine::local_copy::LocalCopyPlan;

/// Serialises env mutation across tests in this binary. The standard
/// library makes `set_var`/`remove_var` unsafe in the 2024 edition because
/// they race with concurrent readers; a mutex is the cheapest fix that
/// keeps the test self-contained.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Env vars consumed by the engine spill layer. Names mirror the public
/// contract documented in `docs/design/spill-policy-public-api.md`.
const ENV_THRESHOLD: &str = "OC_RSYNC_SPILL_THRESHOLD_BYTES";
const ENV_DIR: &str = "OC_RSYNC_SPILL_DIR";

/// RAII helper that sets an env var and restores the previous value on
/// drop. Mirrors `platform::env::EnvGuard` but is inlined to avoid adding
/// `platform` to engine's dev-dependencies.
struct EnvGuard {
    key: OsString,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let key_os = OsString::from(key);
        let previous = env::var_os(&key_os);
        // SAFETY: tests sharing this binary serialise on `ENV_LOCK`, so no
        // other thread reads or writes the environment while we mutate it.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(&key_os, value);
        }
        Self {
            key: key_os,
            previous,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the surrounding test owns `ENV_LOCK` until after this
        // guard drops, so no other thread observes the transient state.
        #[allow(unsafe_code)]
        unsafe {
            if let Some(value) = self.previous.take() {
                env::set_var(&self.key, value);
            } else {
                env::remove_var(&self.key);
            }
        }
    }
}

/// Deterministic xorshift64 PRNG. Avoids adding `rand` as a dev-dep just
/// to seed a 10 MB payload.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // xorshift cannot start from zero - any non-zero seed works.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn fill(&mut self, dst: &mut [u8]) {
        let mut i = 0;
        while i + 8 <= dst.len() {
            let bytes = self.next_u64().to_le_bytes();
            dst[i..i + 8].copy_from_slice(&bytes);
            i += 8;
        }
        if i < dst.len() {
            let bytes = self.next_u64().to_le_bytes();
            let remaining = dst.len() - i;
            dst[i..].copy_from_slice(&bytes[..remaining]);
        }
    }
}

/// Returns the number of regular files inside `dir` whose name ends in
/// `.tmp`. The spill layer's tempfile backend uses the `tempfile` crate,
/// which produces names like `.tmp<random>` on Unix.
fn count_spill_tempfiles(dir: &Path) -> usize {
    let Ok(read) = fs::read_dir(dir) else {
        return 0;
    };
    read.filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let bytes = name.as_encoded_bytes();
            // Match both `tmp*` and `.tmp*` prefixes used by tempfile.
            bytes.starts_with(b"tmp")
                || bytes.starts_with(b".tmp")
                || name.to_string_lossy().ends_with(".tmp")
        })
        .count()
}

/// Populates `dir` with `file_count` files each `bytes_per_file` long,
/// filled with deterministic pseudo-random bytes. The total payload is
/// `file_count * bytes_per_file`; with the test defaults that is exactly
/// 10 MiB across 10 files.
fn write_mixed_corpus(dir: &Path, file_count: usize, bytes_per_file: usize) {
    let mut rng = XorShift64::new(0xC0FF_EE15_CAFE_F00D);
    let mut buffer = vec![0u8; bytes_per_file];
    for i in 0..file_count {
        rng.fill(&mut buffer);
        let path = dir.join(format!("file_{i:02}.bin"));
        fs::write(&path, &buffer).expect("write source file");
    }
}

/// Driver shared by the active and ignored entry points so the test body
/// stays in one place. Returns the spill scratch directory and the number
/// of `.tmp` files observed there after the transfer completes.
fn run_spill_e2e() -> (PathBuf, usize) {
    let scratch = tempfile::tempdir().expect("create scratch root");
    let src_dir = scratch.path().join("src");
    let dst_dir = scratch.path().join("dst");
    let spill_dir = scratch.path().join("spill");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&spill_dir).expect("create spill dir");

    // 10 files * 1 MiB == 10 MiB of mixed pseudo-random payload.
    write_mixed_corpus(&src_dir, 10, 1024 * 1024);

    // Acquire env lock for the lifetime of the env-mutating block. Holding
    // the lock past the transfer prevents any concurrent test from
    // observing or perturbing our env state.
    let _env_lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let _threshold_guard = EnvGuard::set(ENV_THRESHOLD, OsStr::new("131072"));
    let _dir_guard = EnvGuard::set(ENV_DIR, spill_dir.as_os_str());

    // Build operands with a trailing slash on the source so we copy the
    // directory contents into `dst`, matching upstream rsync semantics.
    let mut src_operand = src_dir.into_os_string();
    src_operand.push("/");
    let operands = vec![src_operand, dst_dir.clone().into_os_string()];

    let plan = LocalCopyPlan::from_operands(&operands).expect("build copy plan");
    let summary = plan.execute().expect("local copy succeeds");
    assert_eq!(
        summary.files_copied(),
        10,
        "expected 10 source files to land in destination"
    );

    let observed = count_spill_tempfiles(&spill_dir);
    // Keep `scratch` alive until after we read the directory so the
    // TempDir guard does not delete the spill files before we count them.
    let spill_dir_out = spill_dir.clone();
    drop(_threshold_guard);
    drop(_dir_guard);
    drop(_env_lock);
    drop(scratch);
    (spill_dir_out, observed)
}

/// Asserts the spill layer engaged once env-var wiring lands.
///
/// Currently `#[ignore]` because `LocalCopyPlan::execute` does not yet
/// honour `OC_RSYNC_SPILL_THRESHOLD_BYTES` / `OC_RSYNC_SPILL_DIR`. The
/// blocking work tracks under tasks `STN-8` (env parse), `STN-9`, and
/// `STN-10` (engine wiring). Remove the `#[ignore]` once those land.
#[test]
#[ignore = "blocked on env-var wiring: STN-8/STN-9/STN-10"]
fn spill_env_e2e_engages_spill_layer() {
    let (spill_dir, observed) = run_spill_e2e();
    assert!(
        observed > 0,
        "expected at least one .tmp file in {} after transfer, found {}",
        spill_dir.display(),
        observed,
    );
}
