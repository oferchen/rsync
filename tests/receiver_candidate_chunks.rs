//! The batch receiver decides and requests files chunk by chunk in file-list
//! order, as upstream's generator walks the list one entry at a time
//! (generator.c:2803-2818 generate_files() -> recv_generator()). What a peer
//! or the user sees must not depend on where a chunk ends:
//!
//! - `--progress` counts `to-chk` down by file-list position over the whole
//!   list (progress.c:79-82, set_current_file_index() at progress.c:147-156),
//!   never by a count of the files a chunk transfers.
//! - a no-transfer itemize row (NDX + iflags, generator.c:582-593) reaches the
//!   sender in file-list order with the transfer requests, even when the pass
//!   that recorded it ran over the whole list before the first chunk.
//!
//! Measured against rsync 3.5.1 on Linux (`--no-inc-recursive` pull of the
//! tree in `pull_progress_counts_down_by_file_list_position`, with `b/f3`
//! already up to date): `xfr#1, to-chk=5/7`, `xfr#2, to-chk=3/7`,
//! `xfr#3, to-chk=2/7`. Before the fix oc printed `2/3`, `1/3`, `0/3`: the
//! transfer set, not the file list.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The receiver's candidate chunk (`CANDIDATE_CHUNK` in the transfer crate's
/// batch driver). The order test needs a second chunk to exist.
const CANDIDATE_CHUNK: usize = 8192;

/// One mtime for every fixture entry, so equal copies pass the quick check.
fn mtime() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Writes `body` to `path` with a fixed mtime, so a copy written the same way
/// passes the quick check.
fn write_file(path: &Path, body: &[u8]) {
    fs::write(path, body).expect("write file");
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(mtime()))
        .expect("set mtime");
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host and exec the
/// command locally, so the remote side really is a separate `--server` process.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    set_mode(&script, 0o755);
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    rsh: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("dst")).expect("create dst");
        let rsh = write_rsh_shim(&root);
        Self {
            _temp: temp,
            root,
            rsh,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Runs `args` with the shim as the remote shell and `h:` as the host.
    fn run(&self, args: &[&str]) -> String {
        let out = test_support::OcRsyncCliRunner::new()
            .binary(oc_rsync_binary())
            .cwd(&self.root)
            .arg("-e")
            .arg(&self.rsh)
            .arg(format!("--rsync-path={}", oc_rsync_binary().display()))
            .args(args)
            .run()
            .expect("transfer did not finish");
        out.assert_success();
        out.stdout_str().into_owned()
    }
}

#[test]
fn pull_progress_counts_down_by_file_list_position() {
    let fx = Fixture::new();
    for dir in ["src/a", "src/b", "dst/b"] {
        fs::create_dir_all(fx.path(dir)).expect("create dir");
    }
    write_file(&fx.path("src/a/f1"), b"1");
    write_file(&fx.path("src/a/f2"), b"22");
    write_file(&fx.path("src/b/f3"), b"333");
    write_file(&fx.path("src/top"), b"4444");
    // Up to date, so the transfer set (3) is smaller than the file list (7).
    write_file(&fx.path("dst/b/f3"), b"333");

    let stdout = fx.run(&["-a", "--progress", "--no-inc-recursive", "h:src/", "dst/"]);
    let counters: Vec<&str> = stdout
        .split(['\r', '\n'])
        .filter_map(|line| line.find("(xfr#").map(|at| &line[at..]))
        .collect();
    assert_eq!(
        counters,
        [
            "(xfr#1, to-chk=5/7)",
            "(xfr#2, to-chk=3/7)",
            "(xfr#3, to-chk=2/7)",
        ],
        "to-chk must count down by file-list position over all 7 entries, as \
         3.5.1 does (progress.c:79-82)\nstdout:\n{stdout}"
    );
}

#[test]
fn push_itemize_rows_stay_in_file_list_order_across_candidate_chunks() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.path("src/d/zz")).expect("create src/d/zz");
    fs::create_dir_all(fx.path("dst/d/zz")).expect("create dst/d/zz");
    // File list: ".", "d", d/f00000.., "d/zz". With CANDIDATE_CHUNK + 8 files
    // the last files and "d/zz" fall in the second chunk.
    let files = CANDIDATE_CHUNK + 8;
    let perms_only = format!("f{:05}", files - 2);
    let missing = format!("f{:05}", files - 1);
    for i in 0..files {
        let name = format!("f{i:05}");
        write_file(&fx.path(&format!("src/d/{name}")), name.as_bytes());
        if name != missing {
            write_file(&fx.path(&format!("dst/d/{name}")), name.as_bytes());
        }
    }
    // A file row recorded by the second chunk's candidate pass, then that
    // chunk's transfer request, then a directory row recorded by the
    // directory pass before any chunk ran.
    set_mode(&fx.path(&format!("src/d/{perms_only}")), 0o644);
    set_mode(&fx.path(&format!("dst/d/{perms_only}")), 0o600);
    set_mode(&fx.path("src/d/zz"), 0o755);
    set_mode(&fx.path("dst/d/zz"), 0o700);
    for dir in ["src/d", "dst/d", "src/d/zz", "dst/d/zz"] {
        fs::File::open(fx.path(dir))
            .and_then(|f| f.set_modified(mtime()))
            .expect("set dir mtime");
    }

    let stdout = fx.run(&["-ai", "--no-inc-recursive", "src/", "h:dst/"]);
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|line| {
            line.ends_with(&perms_only) || line.ends_with(&missing) || line.ends_with("d/zz/")
        })
        .collect();
    assert_eq!(
        rows,
        [
            format!(".f...p..... d/{perms_only}").as_str(),
            format!("<f+++++++++ d/{missing}").as_str(),
            ".d...p..... d/zz/",
        ],
        "the receiver must send each itemize row in file-list order with the \
         transfer requests (generator.c:582-593), whichever chunk holds it\n\
         stdout:\n{stdout}"
    );
}
