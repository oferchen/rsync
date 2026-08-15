//! A per-directory merge file that names itself must not recurse forever.
//!
//! upstream: exclude.c:359-375 `add_rule` - "If the local merge file was
//! already mentioned, don't add it again." Registering a dir-merge file is
//! idempotent, so `: .rsync-filter` written inside `.rsync-filter` is accepted
//! and the transfer completes. Measured against rsync 3.5.0: exit 0, and the
//! file list is the same as with an empty filter file.
//!
//! Before the fix oc re-registered the merge file on every pass of the growing
//! push loop (`context_impl/transfer.rs`) and never terminated.
//!
//! ⚠ Every case here runs under a wall-clock bound. A regression must surface
//! as a FAILED test, not as a CI job that hangs until the runner times out - an
//! unbounded assertion would reproduce the very symptom it guards against.

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Generous enough that a loaded CI runner never trips it, short enough that a
/// genuine infinite loop is caught quickly.
const RUN_BUDGET: Duration = Duration::from_secs(60);

/// Polls `child` until it exits or `RUN_BUDGET` elapses.
///
/// Returns the exit code, or `None` when the budget expired (the process is
/// killed and reaped so the test never leaks it).
fn wait_bounded(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + RUN_BUDGET;
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => return Some(status.code().unwrap_or(-1)),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Builds `src/{a.txt,sub/f.txt}` plus a `.rsync-filter` holding `contents`.
fn fixture(contents: &str) -> TempDir {
    let root = TempDir::new().expect("tempdir");
    let src = root.path().join("src");
    fs::create_dir_all(src.join("sub")).expect("create src/sub");
    fs::create_dir_all(root.path().join("dst")).expect("create dst");
    fs::write(src.join("a.txt"), b"a\n").expect("write a.txt");
    fs::write(src.join("sub/f.txt"), b"f\n").expect("write f.txt");
    fs::write(src.join(".rsync-filter"), contents).expect("write .rsync-filter");
    root
}

/// Runs `-n -a -F src/ dst/`, returning the exit code and stdout.
fn run_dir_merge(root: &Path) -> (Option<i32>, String) {
    let mut child = Command::new(oc_rsync_bin())
        .arg("-n")
        .arg("-a")
        .arg("-F")
        .arg("--out-format=%n")
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("{}/", root.join("dst").display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn oc-rsync");

    // Take stdout before waiting: a filled pipe buffer would otherwise block
    // the child and be indistinguishable from the hang under test.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || {
        use std::io::Read as _;
        let mut buffer = String::new();
        let _ = stdout.read_to_string(&mut buffer);
        buffer
    });

    let code = wait_bounded(&mut child);
    let output = reader.join().unwrap_or_default();
    (code, output)
}

/// Sorted transferred names, so listings compare independently of order.
fn names(output: &str) -> Vec<String> {
    let mut names: Vec<String> = output.lines().map(str::to_owned).collect();
    names.sort();
    names
}

#[test]
fn dir_merge_naming_itself_terminates_and_matches_an_inert_filter() {
    // `: .rsync-filter` is a VALID rule - a dir-merge naming the file `-F`
    // already merges. Upstream accepts it (exit 0) because re-registering an
    // active merge file is a silent no-op.
    let recursive = fixture(": .rsync-filter\n");
    let (code, output) = run_dir_merge(recursive.path());
    assert_eq!(
        code,
        Some(0),
        "a self-referential dir-merge must complete like upstream; \
         None means it exceeded {RUN_BUDGET:?} and was killed"
    );

    // The rule registers a merge file and contributes no patterns, so the
    // listing must equal the one produced by a filter file with no rules.
    let inert = fixture("# no rules\n");
    let (inert_code, inert_output) = run_dir_merge(inert.path());
    assert_eq!(inert_code, Some(0), "control run must complete");
    assert_eq!(
        names(&output),
        names(&inert_output),
        "a self-reference must not change which files transfer"
    );
}

#[test]
fn mutually_recursive_dir_merges_terminate() {
    // `.rsync-filter` names `other.f`, which names `.rsync-filter` back.
    let root = fixture(": other.f\n");
    fs::write(root.path().join("src/other.f"), ": .rsync-filter\n").expect("write other.f");
    let (code, _) = run_dir_merge(root.path());
    assert_eq!(
        code,
        Some(0),
        "mutual recursion between two merge files must terminate"
    );
}

#[test]
fn a_merge_file_naming_a_distinct_file_still_loads_it() {
    // Non-vacuity: the registry must not suppress a DIFFERENT merge file.
    // `other.f` excludes a.txt, so a working registration removes it from the
    // listing - if dedup were too aggressive, a.txt would still be there.
    let root = fixture(": other.f\n");
    fs::write(root.path().join("src/other.f"), "- a.txt\n").expect("write other.f");
    let (code, output) = run_dir_merge(root.path());
    assert_eq!(code, Some(0), "distinct merge file must load and complete");
    let listed = names(&output);
    assert!(
        !listed.iter().any(|name| name == "a.txt"),
        "the nested merge file's exclude must apply, got {listed:?}"
    );
    assert!(
        listed.iter().any(|name| name == "sub/f.txt"),
        "unrelated files must still transfer, got {listed:?}"
    );
}
