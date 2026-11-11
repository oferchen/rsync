//! oc's transfer-pipeline internals print only under the oc-only PIPELINE
//! debug word.
//!
//! Upstream has no phase timings, call totals or segment lines, so no `-v`
//! count may print them: its `-v` rows (options.c:244-252
//! `debug_verbosity[]`) name only upstream's own words. `--debug=pipeline`
//! still shows them, level 1 the summaries and level 2 the detail.
#![cfg(unix)]
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use filetime::FileTime;
use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

/// One line from each family of pipeline internals, by level.
const LEVEL_ONE: [&str; 4] = [
    "file-list-sort: ",
    "generator-transfer-loop: ",
    "totals: calls=",
    "partitioned file list: ",
];
const LEVEL_TWO: [&str; 3] = [
    "reclaiming segment ",
    "inc_recurse producer: ",
    "remote-shell wire: ",
];

/// Copies a small nested tree, which recurses incrementally over several
/// sub-lists, and returns stdout and stderr together.
fn copy_with(args: &[&str]) -> String {
    copy_tree(&oc_rsync_bin(), args).0
}

/// Runs `binary -a ARGS` over a fresh tree whose `e/` subtree is already up
/// to date in the destination. Returns the output and the temp root.
fn copy_tree(binary: &Path, args: &[&str]) -> (String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("d1/d2")).unwrap();
    fs::create_dir_all(src.join("e")).unwrap();
    fs::write(src.join("f1"), "a").unwrap();
    fs::write(src.join("d1/f2"), "bb").unwrap();
    fs::write(src.join("d1/d2/f3"), "ccc").unwrap();
    fs::write(src.join("e/f4"), "dddd").unwrap();
    symlink("f1", src.join("l1")).unwrap();
    let dst = tmp.path().join("dst");
    let seeded = Command::new(oc_rsync_bin())
        .arg("-a")
        .arg(src.join("e"))
        .arg(&dst)
        .status()
        .unwrap();
    assert!(seeded.success());
    // The seed creates `dst` after `src` is built; once that crosses a
    // second, 3.5.1 and oc both list `./`. Each binary gets its own tree, so
    // pin both roots to one mtime and keep the listing independent of timing.
    let pinned = FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&src, pinned).unwrap();
    filetime::set_file_mtime(&dst, pinned).unwrap();
    let output = Command::new(binary)
        .arg("-a")
        .args(args)
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (text, tmp.path().display().to_string())
}

/// The lines of `text` with the temp root replaced and every number and
/// mkstemp suffix masked, so runs compare by line shape. A digit run swallows
/// the `,` and `.` inside it: 3.5.1 groups the rate's thousands and oc does
/// not yet (filed apart).
fn line_shapes(text: &str, root: &str) -> BTreeSet<String> {
    text.replace(root, "ROOT")
        .lines()
        .map(|line| {
            let line = mask_source_site(&mask_temp_names(line));
            let mut shape = String::with_capacity(line.len());
            let mut in_number = false;
            for c in line.chars() {
                let numeric = c.is_ascii_digit() || (in_number && (c == ',' || c == '.'));
                if numeric {
                    if !in_number {
                        shape.push('N');
                    }
                } else {
                    shape.push(c);
                }
                in_number = numeric;
            }
            shape
        })
        .collect()
}

/// Replaces the source file `_exit_cleanup` names: oc names the deciding
/// Rust site where 3.5.1 names its C one, as its `at FILE(LINE)` trailer does.
fn mask_source_site(line: &str) -> String {
    match line.split_once(" file=") {
        Some((head, tail)) if head.contains("_exit_cleanup(") => match tail.split_once(',') {
            Some((_, rest)) => format!("{head} file=FILE,{rest}"),
            None => line.to_owned(),
        },
        _ => line.to_owned(),
    }
}

/// Replaces the random part of each receiver temp name, `.NAME.XXXXXX`
/// (receiver.c get_tmpname), with a fixed marker.
fn mask_temp_names(line: &str) -> String {
    line.split(' ')
        .map(|word| {
            let (dir, base) = word.rsplit_once('/').map_or(("", word), |(d, b)| (d, b));
            let Some(stem) = base.strip_prefix('.') else {
                return word.to_owned();
            };
            match stem.rsplit_once('.') {
                Some((name, suffix))
                    if !name.is_empty()
                        && suffix.len() == 6
                        && suffix.bytes().all(|b| b.is_ascii_alphanumeric()) =>
                {
                    let sep = if dir.is_empty() && !word.contains('/') {
                        ""
                    } else {
                        "/"
                    };
                    format!("{dir}{sep}.{name}.XXXXXX")
                }
                _ => word.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn no_verbosity_prints_a_pipeline_line() {
    let out = copy_with(&["-vvvvv"]);
    for line in LEVEL_ONE.iter().chain(&LEVEL_TWO) {
        assert!(!out.contains(line), "-vvvvv printed {line:?}:\n{out}");
    }
}

#[test]
fn the_pipeline_word_prints_its_lines_by_level() {
    let one = copy_with(&["--debug=pipeline"]);
    for line in LEVEL_ONE {
        assert!(
            one.contains(line),
            "--debug=pipeline lacks {line:?}:\n{one}"
        );
    }
    for line in LEVEL_TWO {
        assert!(
            !one.contains(line),
            "--debug=pipeline printed level-2 {line:?}:\n{one}"
        );
    }
    let two = copy_with(&["--debug=pipeline2"]);
    for line in LEVEL_ONE.iter().chain(&LEVEL_TWO) {
        assert!(
            two.contains(line),
            "--debug=pipeline2 lacks {line:?}:\n{two}"
        );
    }
}

/// Every line a local `-vvv` copy prints, 3.5.1 prints too, except the
/// kernel sandbox decisions: SECCOMP and LANDLOCK join the `-vvv` and
/// `-vvvv` rows by the 2026-10-14 decision, and 3.5.1 has no sandbox.
#[test]
fn local_vvv_prints_no_oc_only_line_but_the_sandbox_ones() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    for verbosity in ["-vvv", "-vvvv"] {
        let (oc, oc_root) = copy_tree(&oc_rsync_bin(), &[verbosity]);
        let (up, up_root) = copy_tree(upstream.binary(), &[verbosity]);
        let upstream_shapes = line_shapes(&up, &up_root);
        let oc_only: Vec<String> = line_shapes(&oc, &oc_root)
            .into_iter()
            .filter(|line| !upstream_shapes.contains(line))
            .filter(|line| !line.starts_with("landlock: ") && !line.starts_with("seccomp: "))
            .collect();
        assert!(
            oc_only.is_empty(),
            "{verbosity} lines 3.5.1 never prints: {oc_only:#?}"
        );
    }
}

/// Upstream's per-role `-vvv` trace lines oc has ported, by line prefix.
const PORTED_ROLE_LINES: [&str; 5] = [
    "server_recv(",
    "recv_files",
    "renaming ",
    "generate_files finished",
    "[sender] _exit_cleanup(",
];

/// A local `-vvv` copy prints every ported role line 3.5.1 prints: the
/// receiver's per-entry, rename and start/finish traces (main.c:1199,
/// receiver.c:901/1394, rsync.c:917), the generator's finish
/// (generator.c:2928) and the client's exit trace (cleanup.c:231-236).
#[test]
fn local_vvv_prints_the_ported_upstream_role_lines() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let (oc, oc_root) = copy_tree(&oc_rsync_bin(), &["-vvv"]);
    let (up, up_root) = copy_tree(upstream.binary(), &["-vvv"]);
    let oc_shapes = line_shapes(&oc, &oc_root);
    let upstream_shapes = line_shapes(&up, &up_root);
    for prefix in PORTED_ROLE_LINES {
        assert!(
            upstream_shapes.iter().any(|line| line.starts_with(prefix)),
            "3.5.1 printed no {prefix:?} line:\n{up}"
        );
    }
    let missing: Vec<&String> = upstream_shapes
        .iter()
        .filter(|line| PORTED_ROLE_LINES.iter().any(|p| line.starts_with(p)))
        .filter(|line| !oc_shapes.contains(*line))
        .collect();
    assert!(missing.is_empty(), "-vvv lacks 3.5.1's {missing:#?}:\n{oc}");
    // The shapes mask numbers; argc is not one that varies per run.
    assert_eq!(server_recv_argc(&oc), server_recv_argc(&up));
}

/// The argc of the `server_recv(%d) starting` line in `text`.
fn server_recv_argc(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("server_recv("))
        .and_then(|rest| rest.split_once(')'))
        .map(|(argc, _)| argc)
}

/// The client's `_exit_cleanup` trace with its source site masked: what
/// remains is the role, the first and final codes and the dry-run suffix.
fn exit_trace(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.contains("_exit_cleanup("))?;
    let (head, tail) = line.split_once(" file=")?;
    let (_, rest) = tail.split_once("): ")?;
    Some(format!("{head} {rest}"))
}

/// The exit trace names the same codes as 3.5.1's: a clean run, a dry run,
/// and a run whose missing source turns a clean finish into exit 23 while
/// the first code stays 0 (cleanup.c:210-218, :231-236).
#[test]
fn local_vvv_exit_trace_names_3_5_1_codes() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("f"), "a").unwrap();
    let cases: [(&str, Vec<String>); 3] = [
        ("clean", vec![format!("{}/", src.display())]),
        (
            "dry run",
            vec!["-n".to_owned(), format!("{}/", src.display())],
        ),
        (
            "missing source",
            vec![tmp.path().join("nope").display().to_string()],
        ),
    ];
    for (name, operands) in cases {
        let trace = |binary: &Path, dst: &str| {
            let output = Command::new(binary)
                .args(["-a", "-vvv"])
                .args(&operands)
                .arg(tmp.path().join(dst))
                .output()
                .unwrap();
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            exit_trace(&text)
        };
        let oc = trace(&oc_rsync_bin(), &format!("oc-{name}"));
        let up = trace(upstream.binary(), &format!("up-{name}"));
        assert!(up.is_some(), "{name}: 3.5.1 printed no exit trace");
        assert_eq!(oc, up, "{name}");
    }
}
