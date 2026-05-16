//! Stress test for nested `.rsync-filter` inheritance against upstream rsync.
//!
//! Builds a deep source tree (four directory levels) and seeds each level with
//! a `.rsync-filter` that mixes include/exclude rules, `merge` and `dir-merge`
//! directives, and the leading `+`/`-` modifiers. The test then runs both
//! upstream rsync 3.4.2 and oc-rsync in dry-run mode with `-av -F` and asserts
//! that the produced file lists agree.
//!
//! The dry-run output is parsed into the set of file/directory paths emitted by
//! rsync between the "sending incremental file list" banner and the trailing
//! "sent ..." / "total ..." statistics. Both implementations must visit the
//! same paths after dir-merge inheritance, child re-inclusion, and grandchild
//! re-exclusion have been applied.
//!
//! ## Rule interactions exercised
//!
//! - Root `.rsync-filter` excludes `*.tmp` globally.
//! - Level-1 `.rsync-filter` re-includes `*.tmp` via a leading `+` rule.
//! - Level-2 `.rsync-filter` re-excludes a specific `secret.tmp` via `-`.
//! - Level-3 `.rsync-filter` adds a `dir-merge` directive pointing at a
//!   per-directory `.extra-filter` file that further excludes `*.bak`.
//! - A top-level `merge` directive pulls extra exclude rules from a sibling
//!   file (`extra.rules`) to confirm `merge` and `dir-merge` cooperate.
//! - Anchored (`/`-prefixed) and unanchored patterns appear together to
//!   exercise the anchor semantics inherited across directory levels.
//!
//! The test gates on the presence of the upstream rsync binary. The expected
//! path is `target/interop/upstream-install/3.4.2/bin/rsync`, with a fallback
//! to the source tree at `target/interop/upstream-src/rsync-3.4.2/rsync` so
//! that local interop builds (which leave the binary in the source dir) are
//! also picked up. When neither location holds a runnable binary, the test
//! prints a skip message and returns successfully so that CI environments
//! without the upstream interop fixture stay green.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Candidate locations for the upstream rsync 3.4.2 binary.
const UPSTREAM_CANDIDATES: &[&str] = &[
    "target/interop/upstream-install/3.4.2/bin/rsync",
    "target/interop/upstream-src/rsync-3.4.2/rsync",
];

/// Default timeout for spawned rsync processes.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);

/// Locate the upstream rsync 3.4.2 binary, walking upward from the test cwd
/// so the lookup works regardless of which crate triggered the test.
fn locate_upstream() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        for candidate in UPSTREAM_CANDIDATES {
            let path = ancestor.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Build a nested source tree with `.rsync-filter` files at four depth levels.
///
/// Returns the path to the constructed source root (caller is responsible for
/// keeping the parent `TestDir` alive for the lifetime of the test).
fn build_nested_fixture(src_root: &Path) -> std::io::Result<()> {
    // ---- Level 0 (root) ----
    // Global exclude for *.tmp, plus a `merge` directive that imports a
    // sibling rules file for extra excludes.
    fs::write(
        src_root.join(".rsync-filter"),
        concat!(
            ". extra.rules\n",
            "- *.tmp\n",
            "+ keep-me.tmp\n",
            ": .rsync-filter\n",
        )
        .as_bytes(),
    )?;
    fs::write(src_root.join("extra.rules"), b"- *.swp\n- *.orig\n")?;
    fs::write(src_root.join("keep-me.tmp"), b"root keep")?;
    fs::write(src_root.join("drop-me.tmp"), b"root drop")?;
    fs::write(src_root.join("notes.txt"), b"root notes")?;
    fs::write(src_root.join("scratch.swp"), b"swap")?;

    // ---- Level 1 ----
    let l1 = src_root.join("level1");
    fs::create_dir_all(&l1)?;
    // Re-include all *.tmp under level1, but keep the global *.swp exclude.
    fs::write(
        l1.join(".rsync-filter"),
        concat!("+ *.tmp\n", "- /private/\n",).as_bytes(),
    )?;
    fs::write(l1.join("alpha.tmp"), b"alpha")?;
    fs::write(l1.join("beta.txt"), b"beta")?;
    fs::write(l1.join("draft.swp"), b"draft swap")?;
    fs::create_dir_all(l1.join("private"))?;
    fs::write(l1.join("private/secret.txt"), b"secret")?;

    // ---- Level 2 ----
    let l2 = l1.join("level2");
    fs::create_dir_all(&l2)?;
    // Re-exclude a specific *.tmp; keep other tmp files re-included by level1.
    fs::write(
        l2.join(".rsync-filter"),
        concat!("- secret.tmp\n", "+ *.cfg\n", "- *.cfg.bak\n",).as_bytes(),
    )?;
    fs::write(l2.join("secret.tmp"), b"secret tmp")?;
    fs::write(l2.join("public.tmp"), b"public tmp")?;
    fs::write(l2.join("app.cfg"), b"app cfg")?;
    fs::write(l2.join("app.cfg.bak"), b"app cfg bak")?;
    fs::write(l2.join("readme.md"), b"readme")?;

    // ---- Level 3 ----
    let l3 = l2.join("level3");
    fs::create_dir_all(&l3)?;
    // Add a dir-merge that pulls rules from `.extra-filter` in this dir.
    fs::write(
        l3.join(".rsync-filter"),
        concat!(":- .extra-filter\n", "+ *.keep\n",).as_bytes(),
    )?;
    fs::write(l3.join(".extra-filter"), b"*.bak\n*.log\n")?;
    fs::write(l3.join("important.keep"), b"keep")?;
    fs::write(l3.join("stale.bak"), b"bak")?;
    fs::write(l3.join("trace.log"), b"log")?;
    fs::write(l3.join("plain.txt"), b"plain")?;
    fs::write(l3.join("data.tmp"), b"tmp inherited")?;

    // ---- Level 4 ----
    let l4 = l3.join("level4");
    fs::create_dir_all(&l4)?;
    // No local filter file; everything here is governed by inherited rules.
    fs::write(l4.join("deep.txt"), b"deep")?;
    fs::write(l4.join("deep.tmp"), b"deep tmp")?;
    fs::write(l4.join("deep.bak"), b"deep bak")?;
    fs::write(l4.join("deep.cfg"), b"deep cfg")?;

    Ok(())
}

/// Run a command with a deadline and capture stdout. Panics on timeout because
/// a hung rsync invocation indicates a real defect worth surfacing in CI.
fn run_capture(mut command: Command, label: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let start = std::time::Instant::now();

    loop {
        match child.try_wait()? {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut s) = child.stdout.take() {
                    s.read_to_end(&mut stdout)?;
                }
                if let Some(mut s) = child.stderr.take() {
                    s.read_to_end(&mut stderr)?;
                }
                if !status.success() {
                    panic!(
                        "{label} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
                        status.code(),
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr),
                    );
                }
                return Ok(stdout);
            }
            None => {
                if start.elapsed() >= SPAWN_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{label} exceeded timeout of {SPAWN_TIMEOUT:?}");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

/// Extract the set of transferred entries from rsync's verbose dry-run output.
///
/// rsync emits a banner ("sending incremental file list"), then one entry per
/// transferred path, then a blank line, then the summary stats. We keep lines
/// that look like path entries and drop the banner, the trailing summary, the
/// "created directory" line, and any blank lines. The result is sorted so the
/// comparison is independent of traversal order.
fn parse_file_list(raw: &[u8]) -> BTreeSet<String> {
    let text = String::from_utf8_lossy(raw);
    let mut entries = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with("sending incremental file list") {
            continue;
        }
        if line.starts_with("sent ")
            || line.starts_with("total ")
            || line.starts_with("created directory ")
            || line.starts_with("delta-transmission ")
        {
            continue;
        }
        // Skip itemize lines if any leaked in (we did not request -i, but be safe).
        if line.starts_with("cd+") || line.starts_with(">f") {
            continue;
        }
        entries.insert(line.to_string());
    }
    entries
}

#[test]
fn nested_rsync_filter_inheritance_matches_upstream() {
    let upstream = match locate_upstream() {
        Some(path) => path,
        None => {
            eprintln!(
                "Skipping nested_rsync_filter_inheritance_matches_upstream: \
                 upstream rsync 3.4.2 binary not found at any of {UPSTREAM_CANDIDATES:?}"
            );
            return;
        }
    };

    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("create src dir");
    let dest_upstream = test_dir
        .mkdir("dest_upstream")
        .expect("create upstream dest");
    let dest_oc = test_dir.mkdir("dest_oc").expect("create oc dest");

    build_nested_fixture(&src).expect("build nested fixture");

    let src_arg = format!("{}/", src.display());

    // --- Run upstream rsync ---
    let mut upstream_cmd = Command::new(&upstream);
    upstream_cmd.args([
        "--dry-run",
        "-av",
        "-F",
        &src_arg,
        &format!("{}/", dest_upstream.display()),
    ]);
    let upstream_stdout = run_capture(upstream_cmd, "upstream rsync").expect("run upstream rsync");
    let upstream_entries = parse_file_list(&upstream_stdout);

    // --- Run oc-rsync ---
    let mut oc_cmd = RsyncCommand::new();
    oc_cmd.args([
        "--dry-run",
        "-av",
        "-F",
        &src_arg,
        &format!("{}/", dest_oc.display()),
    ]);
    let oc_output = oc_cmd.run().expect("run oc-rsync");
    assert!(
        oc_output.status.success(),
        "oc-rsync should exit 0 (stderr: {})",
        String::from_utf8_lossy(&oc_output.stderr)
    );
    let oc_entries = parse_file_list(&oc_output.stdout);

    // --- Compare line-by-line ---
    if upstream_entries != oc_entries {
        let only_upstream: Vec<_> = upstream_entries.difference(&oc_entries).collect();
        let only_oc: Vec<_> = oc_entries.difference(&upstream_entries).collect();
        panic!(
            "Nested .rsync-filter file lists diverge.\n\
             Only in upstream: {only_upstream:#?}\n\
             Only in oc-rsync: {only_oc:#?}\n\
             Upstream raw stdout:\n{}\n\
             oc-rsync raw stdout:\n{}\n",
            String::from_utf8_lossy(&upstream_stdout),
            String::from_utf8_lossy(&oc_output.stdout),
        );
    }

    // Sanity: at minimum, the inheritance chain should leave a known-good entry
    // (`level1/level2/level3/important.keep`) transferred by both sides. If
    // both binaries agree but produced an empty list, something is wrong.
    assert!(
        upstream_entries
            .iter()
            .any(|e| e.contains("level1/level2/level3/important.keep")),
        "expected important.keep to survive the filter chain in upstream output: \
         {upstream_entries:?}"
    );
}
