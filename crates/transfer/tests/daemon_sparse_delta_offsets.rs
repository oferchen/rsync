//! Pins byte-exact `--sparse` delta reconstruction on the network receiver.
//!
//! Under `--sparse`, a token whose data ends in zero bytes leaves those zeros
//! as a pending hole with the output positioned at the start of the run. The
//! next token must be written from there: the hole is flushed by advancing
//! from the current position. Any seek to the logical output offset while the
//! run is pending makes the hole start late and shifts every following byte.
//! The local-copy engine had exactly that defect under `--inplace --sparse`.
//!
//! Upstream never repositions between tokens: receiver.c:563 `write_file()`
//! and receiver.c:625 `skip_matched()` both hand the bytes to fileio.c
//! `write_sparse()`, whose fileio.c:81 `flush_sparse_hole()` advances from the
//! current position before fileio.c:115 `emit_sparse_span()` writes the data.
//!
//! These cells drive the daemon receiver (push) and the client receiver (pull)
//! over a real `rsync://` loopback with a pinned block size, so a matched block
//! ending in zeros is followed by a literal, and a zero literal by a matched
//! block. `--stats` must report matched data, otherwise the delta path was not
//! exercised and the cell proves nothing.

#![cfg(unix)]

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

const BLOCK: usize = 1024;
const LEN: usize = 64 * BLOCK;

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(oc_bin: &Path, config: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard(child), port))
}

/// Non-zero pseudo-random bytes, except where the fixture plants zeros.
fn basis() -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let mut basis: Vec<u8> = (0..LEN)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((seed >> 16) as u8) | 1
        })
        .collect();
    // Block 15 ends in one zero byte, block 39 in a nine-byte zero run.
    basis[16 * BLOCK - 1] = 0;
    basis[40 * BLOCK - 9..40 * BLOCK].fill(0);
    basis
}

/// The basis with a non-zero literal after block 15 and a zero literal after
/// block 39, each followed by matched blocks again.
fn source_from(basis: &[u8]) -> Vec<u8> {
    let mut source = basis.to_vec();
    for byte in &mut source[16 * BLOCK..20 * BLOCK] {
        *byte = !*byte | 1;
    }
    source[40 * BLOCK..48 * BLOCK].fill(0);
    source
}

fn backdate(path: &Path) {
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open for backdate")
        .set_modified(past)
        .expect("backdate");
}

/// Parses `Matched data: N bytes` from `--stats` output.
fn matched_bytes(stdout: &str) -> u64 {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Matched data:"))
        .unwrap_or_else(|| panic!("no `Matched data:` line in --stats output:\n{stdout}"));
    line.chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("matched byte count")
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Push,
    Pull,
}

/// Transfers `source` over `basis` through the daemon and returns the
/// reconstructed destination plus the reported matched byte count.
fn transfer(direction: Direction, flags: &[&str], source: &[u8], basis: &[u8]) -> (Vec<u8>, u64) {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();
    let (src_dir, dest_dir) = (root.join("src"), root.join("dest"));
    fs::create_dir_all(&src_dir).expect("create source dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let (src_file, dest_file) = (src_dir.join("f.bin"), dest_dir.join("f.bin"));
    fs::write(&src_file, source).expect("write source");
    fs::write(&dest_file, basis).expect("write basis");
    backdate(&dest_file);

    let module_root = match direction {
        Direction::Push => &dest_dir,
        Direction::Pull => &src_dir,
    };
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nlog file = {log}\nuse chroot = false\n\n\
             [data]\npath = {path}\nread only = false\n",
            pid = root.join("rsyncd.pid").display(),
            log = root.join("rsyncd.log").display(),
            path = module_root.display(),
        ),
    )
    .expect("write daemon config");
    let (_daemon, port) = spawn_daemon(&oc_bin, &config)
        .unwrap_or_else(|e| panic!("could not start the daemon, nothing measured: {e}"));

    let url = format!("rsync://127.0.0.1:{port}/data/f.bin");
    let (from, to) = match direction {
        Direction::Push => (src_file.display().to_string(), url),
        Direction::Pull => (url, dest_file.display().to_string()),
    };
    let output = Command::new(&oc_bin)
        .args([
            "--ignore-times",
            "--no-whole-file",
            "--block-size=1024",
            "--stats",
        ])
        .args(flags)
        .arg(&from)
        .arg(&to)
        .stdin(Stdio::null())
        .output()
        .expect("run oc-rsync client");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{direction:?} {flags:?} exited {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    (
        fs::read(&dest_file).expect("read destination"),
        matched_bytes(&stdout),
    )
}

fn assert_identical(produced: &[u8], expected: &[u8], what: &str) {
    assert_eq!(produced.len(), expected.len(), "{what}: length preserved");
    let first_diff = produced.iter().zip(expected).position(|(a, b)| a != b);
    assert_eq!(
        first_diff, None,
        "{what}: destination must equal the source"
    );
}

/// `--inplace --sparse` and `--sparse` (temp file) reconstruct the source
/// byte-for-byte on both receivers when matched blocks end in zeros.
#[test]
fn sparse_delta_keeps_offsets_after_blocks_ending_in_zeros() {
    let basis = basis();
    let source = source_from(&basis);
    for direction in [Direction::Push, Direction::Pull] {
        for flags in [&["--inplace", "--sparse"][..], &["--sparse"][..]] {
            let what = format!("{direction:?} {flags:?}");
            let (produced, matched) = transfer(direction, flags, &source, &basis);
            assert!(
                matched > 0,
                "{what}: no matched data, delta path not exercised"
            );
            assert_identical(&produced, &source, &what);
        }
    }
}

/// `--append-verify --sparse` resumes after a destination prefix that ends in
/// a zero byte; the appended tail must start right after it.
#[test]
fn sparse_append_verify_keeps_offsets_after_prefix_ending_in_zero() {
    let source = source_from(&basis());
    let prefix = &source[..16 * BLOCK];
    assert_eq!(
        prefix.last(),
        Some(&0),
        "fixture: prefix must end in a zero"
    );
    for direction in [Direction::Push, Direction::Pull] {
        let what = format!("{direction:?} --append-verify --sparse");
        let (produced, _) = transfer(direction, &["--append-verify", "--sparse"], &source, prefix);
        assert_identical(&produced, &source, &what);
    }
}
