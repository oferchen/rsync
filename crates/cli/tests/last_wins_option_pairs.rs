//! Oracle cells: `--sparse`, `-4`/`-6`, `--iconv` and the `--stop-*` options
//! combine the way rsync 3.5.2 combines them.
//!
//! upstream: options.c:727-729 (`sparse_files`) and :845-846 (`default_af_hint`)
//! are POPT_ARG_VAL pairs, so the last one wins. `--iconv` stores a string that
//! OPT_NO_ICONV clears (options.c:843-844, :1831-1834), each at its own
//! position. `--stop-after`, `--time-limit` and `--stop-at` all assign
//! `stop_at_utime` after validating their own value (options.c:837-839,
//! :2035-2058), so any invalid value is refused and the last valid one wins.
//! What reaches the peer: `S` in the option cluster (options.c:2961-2962), the
//! remote charset as `--iconv=` (options.c:2975-2983), and `-4`/`-6` before the
//! host when the remote shell is `ssh` (main.c:604-611).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

const FUTURE: &str = "--stop-at=2099-12-31T23:59";

/// What a push did, reduced to the parts these options decide.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// Exit code: 12 when the recording shell closes the connection, 1 when
    /// option parsing refuses the command line.
    code: Option<i32>,
    /// `-4` or `-6` as passed to the `ssh` remote shell, or empty.
    address_family: String,
    /// Whether the server's option cluster carries `S`.
    sparse: bool,
    /// The `--iconv=` argument sent to the server, if any.
    iconv: Option<String>,
}

/// A one-file source and a remote shell named `ssh` that records its argv.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("src/f"), b"a\n").unwrap();
    let ssh = root.join("bin/ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$(dirname \"$0\")/../argv\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Runs a push with `options`, returning what it did and its stderr.
fn run(binary: &Path, options: &[&str], env_iconv: bool) -> (Outcome, String) {
    let dir = fixture();
    let root = dir.path();
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .env_remove("RSYNC_ICONV")
        .arg("-r")
        .args(options)
        .arg("-e")
        .arg(root.join("bin/ssh"))
        .arg("src/")
        .arg(format!("lh:{}/dst/", root.display()));
    if env_iconv {
        command.env("RSYNC_ICONV", "UTF-8,UTF-8");
    }
    let output = command.output().expect("run push");
    let argv = fs::read_to_string(root.join("argv")).unwrap_or_default();
    let words: Vec<&str> = argv.split_whitespace().collect();
    let host = words.iter().position(|word| *word == "lh");
    let address_family = host
        .map(|host| &words[..host])
        .unwrap_or_default()
        .iter()
        .filter(|word| matches!(**word, "-4" | "-6"))
        .copied()
        .collect();
    let server = words.iter().skip_while(|word| **word != "--server");
    let cluster = server
        .clone()
        .find(|word| word.starts_with('-') && !word.starts_with("--"))
        .map(|word| word.split_once('e').map_or(*word, |(head, _)| head));
    let outcome = Outcome {
        code: output.status.code(),
        address_family,
        sparse: cluster.is_some_and(|letters| letters.contains('S')),
        iconv: server
            .clone()
            .find_map(|word| word.strip_prefix("--iconv="))
            .map(str::to_owned),
    };
    (
        outcome,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn spawned(address_family: &str, sparse: bool, iconv: Option<&str>) -> Outcome {
    Outcome {
        code: Some(12),
        address_family: address_family.to_owned(),
        sparse,
        iconv: iconv.map(str::to_owned),
    }
}

fn refused() -> Outcome {
    Outcome {
        code: Some(1),
        address_family: String::new(),
        sparse: false,
        iconv: None,
    }
}

/// One option order, whether RSYNC_ICONV is set, what rsync 3.5.2 does with
/// it, and the refusal text it prints, if any.
struct Cell {
    options: Vec<&'static str>,
    env_iconv: bool,
    expected: Outcome,
    refusal: Option<&'static str>,
}

fn cell(options: &[&'static str], expected: Outcome) -> Cell {
    Cell {
        options: options.to_vec(),
        env_iconv: false,
        expected,
        refusal: None,
    }
}

fn refusal(options: &[&'static str], text: &'static str) -> Cell {
    Cell {
        options: options.to_vec(),
        env_iconv: false,
        expected: refused(),
        refusal: Some(text),
    }
}

fn cells() -> Vec<Cell> {
    let mut cells = vec![
        cell(&["-S", "--no-S"], spawned("", false, None)),
        cell(&["--no-S", "-S"], spawned("", true, None)),
        cell(&["--sparse", "--no-sparse"], spawned("", false, None)),
        cell(&["--no-sparse", "--sparse"], spawned("", true, None)),
        cell(&["-4", "-6"], spawned("-6", false, None)),
        cell(&["-6", "-4"], spawned("-4", false, None)),
        cell(&["--ipv4", "--ipv6"], spawned("-6", false, None)),
        cell(&["--ipv6", "--ipv4"], spawned("-4", false, None)),
        cell(&["--stop-after=5", FUTURE], spawned("", false, None)),
        cell(&[FUTURE, "--stop-after=5"], spawned("", false, None)),
        cell(&["--time-limit=5", FUTURE], spawned("", false, None)),
        cell(
            &["--stop-after=5", "--time-limit=7"],
            spawned("", false, None),
        ),
        refusal(&["--stop-after=x", FUTURE], "invalid --stop-after value: x"),
        refusal(&[FUTURE, "--stop-after=x"], "invalid --stop-after value: x"),
        refusal(
            &["--stop-after=x", "--stop-after=5"],
            "invalid --stop-after value: x",
        ),
        refusal(
            &["--stop-at=2000-01-01T00:00", "--stop-after=5"],
            "--stop-at time is not in the future: 2000-01-01T00:00",
        ),
        refusal(
            &["--stop-after=5", "--stop-at=bogus"],
            "invalid --stop-at format: bogus",
        ),
    ];
    if cfg!(feature = "iconv") {
        cells.extend([
            cell(
                &["--iconv=UTF-8,UTF-8", "--no-iconv"],
                spawned("", false, None),
            ),
            cell(
                &["--no-iconv", "--iconv=UTF-8,UTF-8"],
                spawned("", false, Some("UTF-8")),
            ),
            cell(
                &["--iconv=UTF-8,latin1", "--iconv=UTF-8,UTF-8"],
                spawned("", false, Some("UTF-8")),
            ),
            Cell {
                options: vec!["--no-iconv"],
                env_iconv: true,
                expected: spawned("", false, None),
                refusal: None,
            },
            Cell {
                options: vec!["--iconv=UTF-8,UTF-8"],
                env_iconv: true,
                expected: spawned("", false, Some("UTF-8")),
                refusal: None,
            },
        ]);
    }
    cells
}

fn check(label: &str, binary: &Path, cell: &Cell) {
    let (outcome, stderr) = run(binary, &cell.options, cell.env_iconv);
    let options = &cell.options;
    assert_eq!(outcome, cell.expected, "{label} {options:?}: {stderr}");
    if let Some(text) = cell.refusal {
        assert!(stderr.contains(text), "{label} {options:?}: {stderr}");
    }
}

#[test]
fn sparse_address_family_iconv_and_stop_options_combine_like_upstream() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for cell in cells() {
        check("oc", &oc_rsync_bin(), &cell);
        if let Some(upstream) = &upstream {
            check("3.5.2", upstream.binary(), &cell);
        }
    }
}
