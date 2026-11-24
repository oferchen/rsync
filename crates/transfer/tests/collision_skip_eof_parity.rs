//! Matched and literal byte counts equal rsync 3.5.2's when a weak-checksum
//! collision skip lands on the basis's short final block.
//!
//! upstream: match.c:366-381 `hash_search()` skips `MIN(k-1, end-1-offset)`
//! bytes after the 4th consecutive false alarm and never tests the window at
//! the landing (match.c:412), so a landing on `end - 1` (match.c:197) leaves
//! the short final block literal. The source is a run of `0x40`; the basis is
//! one 700-byte block with the same weak sum but different bytes, then 100
//! bytes of `0x40`. With every full window a false alarm, the lengths below
//! put the last skip before, exactly on, and just past the tail offset.
//!
//! Each cell runs the same transfer with upstream 3.5.2 and with oc-rsync and
//! compares the `Literal data` and `Matched data` lines of `--stats`. It
//! self-skips unless `OC_RSYNC_UPSTREAM_COMPAT=1` and a 3.5.2 binary resolves.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use filetime::FileTime;
use test_support::file_times::set_file_mtime;
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, UpstreamVersion, create_tempdir, oc_rsync_bin, require_binaries,
    require_upstream_rsync,
};

const FILL: u8 = 0x40;
/// Source lengths where the scan reaches the 100-byte tail normally, and where
/// the last collision skip lands one byte before, exactly on and one byte past
/// its offset.
const LENGTHS: [usize; 4] = [4200, 4301, 4302, 4303];

/// Writes `src/f` (`len` bytes of `FILL`) and an older `dst/f` basis under
/// `root`, so the quick check never skips the file.
fn fixture(root: &Path, len: usize) {
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::create_dir_all(root.join("dst")).expect("mkdir dst");
    let mut basis = vec![FILL; 700];
    basis[300] += 1;
    basis[301] -= 2;
    basis[302] += 1;
    basis.extend(std::iter::repeat_n(FILL, 100));
    fs::write(root.join("dst/f"), basis).expect("write basis");
    fs::write(root.join("src/f"), vec![FILL; len]).expect("write source");
    set_file_mtime(
        root.join("dst/f"),
        FileTime::from_unix_time(1_000_000_000, 0),
    )
    .expect("backdate basis");
}

/// Runs `client` with `args` plus the endpoints and returns its literal and
/// matched byte lines, after checking the copy is byte-exact.
fn counts(client: &Path, root: &Path, args: &[String], remote: bool) -> String {
    let dst = root.join("dst");
    let dest = if remote {
        format!("localhost:{}/", dst.display())
    } else {
        format!("{}/", dst.display())
    };
    let output = Command::new(client)
        .args(["-a", "--no-whole-file", "-B700", "--stats"])
        .args(args)
        .arg(format!("{}/", root.join("src").display()))
        .arg(dest)
        .output()
        .expect("spawn rsync");
    assert!(
        output.status.success(),
        "{client:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dst.join("f")).expect("read copy"),
        fs::read(root.join("src/f")).expect("read source"),
        "{client:?} must reproduce the source"
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("Literal data:") || line.starts_with("Matched data:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compares upstream and oc-rsync counts for every length in [`LENGTHS`].
/// `server_args` builds the extra argv for a given rsync binary.
fn assert_counts_match(remote: bool, server_args: impl Fn(&Path) -> Vec<String>) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    for len in LENGTHS {
        let tmp = create_tempdir();
        let (up_root, oc_root) = (tmp.path().join("up"), tmp.path().join("oc"));
        fixture(&up_root, len);
        fixture(&oc_root, len);
        let expected = counts(
            upstream.binary(),
            &up_root,
            &server_args(upstream.binary()),
            remote,
        );
        let actual = counts(&oc, &oc_root, &server_args(&oc), remote);
        assert_eq!(actual, expected, "source of {len} bytes, remote={remote}");
    }
}

#[test]
fn push_sender_counts_match_upstream() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    assert_counts_match(true, |server| {
        vec![
            format!("--rsh={}", stub.path().display()),
            format!("--rsync-path={}", server.display()),
        ]
    });
}

#[test]
fn local_copy_counts_match_upstream() {
    require_binaries!("oc-rsync");
    assert_counts_match(false, |_| Vec::new());
}
