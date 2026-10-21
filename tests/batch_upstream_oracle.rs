//! Byte-level oracle for the files `--write-batch` produces.
//!
//! Each cell records the same fixture with oc and with upstream rsync 3.5.1,
//! running both from the same working directory with the same relative
//! operands, and compares what lands on disk: whether the batch and its `.sh`
//! exist, the batch header bytes, and the `.sh` text with only `argv[0]`
//! masked (it is the path of whichever binary ran).
//!
//! Every cell also asserts a pinned expectation recorded from rsync 3.5.1, so
//! the cell still fails on a regression where no oracle is installed. When
//! the oracle resolves, the live comparison runs as well; it is located like
//! the other root oracle suites: `OC_RSYNC_UPSTREAM_RSYNC`, then
//! `target/interop/upstream-install/3.5.1/bin/rsync`, then the
//! `upstream-src` build tree, then `/usr/bin/rsync` - each banner-verified
//! as 3.5.1, since only that release defines the expected bytes.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORACLE_VERSION: &str = "3.5.1";

/// The binary under test (`env!`: the variable exists only at compile time).
fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Resolves an rsync whose `--version` banner names [`ORACLE_VERSION`].
fn upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(installed) = test_support::upstream_install_bin(ORACLE_VERSION) {
        candidates.push(installed);
    }
    if let Some(root) = test_support::workspace_root() {
        candidates.push(root.join(format!(
            "target/interop/upstream-src/rsync-{ORACLE_VERSION}/rsync"
        )));
    }
    candidates.push(PathBuf::from("/usr/bin/rsync"));
    let found = candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|banner| banner.lines().next().map(str::to_owned))
            .is_some_and(|first| {
                first.starts_with("rsync") && first.contains(&format!(" version {ORACLE_VERSION}"))
            })
    });
    if found.is_none() {
        eprintln!("no rsync {ORACLE_VERSION} oracle; checking pinned expectations only");
    }
    found
}

/// What one `--write-batch` invocation left behind.
struct Recording {
    output: Output,
    batch: Option<Vec<u8>>,
    script: Option<Vec<u8>>,
}

impl Recording {
    /// The `.sh` with its leading quoted `argv[0]` replaced by `ARGV0`.
    fn masked_script(&self) -> Option<Vec<u8>> {
        let script = self.script.as_ref()?;
        let rest = script.strip_prefix(b"'")?;
        let close = rest.iter().position(|&b| b == b'\'')?;
        let mut masked = b"ARGV0".to_vec();
        masked.extend_from_slice(&rest[close + 1..]);
        Some(masked)
    }
}

/// A fresh source tree plus an empty destination, both relative to `root`.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("dst")).unwrap();
        fs::write(root.join("src/a.txt"), b"alpha\n").unwrap();
        fs::write(root.join("src/sub/b.log"), b"beta\n").unwrap();
        fs::write(root.join("src/sub/c.txt"), b"gamma\n").unwrap();
        Self { _temp: temp, root }
    }

    /// Runs `bin` with `args` from the fixture root.
    fn run(&self, bin: &Path, args: &[&str]) -> Output {
        Command::new(bin)
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()))
    }

    /// Runs `bin` from the fixture root and collects `bf` and `bf.sh`.
    fn record(&self, bin: &Path, args: &[&str]) -> Recording {
        let output = self.run(bin, args);
        Recording {
            output,
            batch: fs::read(self.root.join("bf")).ok(),
            script: fs::read(self.root.join("bf.sh")).ok(),
        }
    }
}

/// Records `args` with oc and, when present, with the oracle - each in its
/// own fresh fixture so neither run can see the other's files.
fn record_both(args: &[&str]) -> (Recording, Option<Recording>) {
    let oc = Fixture::new().record(&oc_rsync(), args);
    let upstream = upstream_rsync().map(|bin| Fixture::new().record(&bin, args));
    (oc, upstream)
}

/// Asserts oc's masked `.sh` equals both the pinned 3.5.1 text and, when an
/// oracle is installed, what the oracle writes for the same invocation.
fn assert_script_matches_upstream(args: &[&str], pinned: &[u8]) {
    let (oc, upstream) = record_both(args);
    assert!(oc.output.status.success(), "{args:?}: {:?}", oc.output);
    let oc_script = oc.masked_script().expect("oc wrote a .sh");
    assert_eq!(
        String::from_utf8_lossy(&oc_script),
        String::from_utf8_lossy(pinned),
        "{args:?}: oc .sh differs from the pinned rsync {ORACLE_VERSION} text"
    );
    if let Some(up) = upstream {
        assert!(
            up.output.status.success(),
            "{args:?}: oracle {:?}",
            up.output
        );
        assert_eq!(
            up.masked_script().as_deref(),
            Some(pinned),
            "{args:?}: the pinned text no longer matches the oracle"
        );
    }
}

/// upstream: batch.c:213-240 write_filter_rules() lists every filter_list
/// entry through get_rule_prefix(for_xfer = 0) (exclude.c:1824-1885). `P` and
/// `R` are `-r`/`+r`, and dir-merge, side, negate and directory modifiers all
/// survive; a here-doc that drops one replays a different rule set than the
/// recording run used.
#[test]
fn filter_heredoc_matches_upstream_get_rule_prefix() {
    assert_script_matches_upstream(
        &[
            "-a",
            "--checksum-seed=7",
            "--write-batch=bf",
            "-f",
            "- gone",
            "-f",
            "!",
            "--exclude=/x",
            "--include=*/",
            "-f",
            "P keep",
            "-f",
            "R junk",
            "-f",
            ":n .rules",
            "-f",
            "- dir/",
            "-f",
            "-! neg",
            "-f",
            "H hid",
            "-f",
            "S sh",
            "-f",
            ":e .m",
            "-f",
            "-x xa",
            "-f",
            "-p per",
            "-f",
            ":C",
            "-f",
            "dir-merge,- .d",
            "-C",
            "--exclude=*.log",
            "src/",
            "dst/",
        ],
        b"ARGV0 --filter='._-' '-a' --checksum-seed='7' --read-batch='bf' '-C' \
${1:-'dst/'} <<'#E#'\n- /x\n+ */\n-r keep\n+r junk\n:n .rules\n- dir/\n-! neg\n\
-s hid\n+s sh\n- .m\n: .m\n-x xa\n-p per\n:C .cvsignore\n:- .d\n- *.log\n#E#\n",
    );
}

/// upstream: batch.c:231-238 - under `--from0` each rule ends in NUL and the
/// block gains a trailing `;\n`, so the replay's `--filter=._-` reads the
/// rules with the same `-0` parsing the recording used.
#[test]
fn filter_heredoc_under_from0_matches_upstream() {
    assert_script_matches_upstream(
        &[
            "-a",
            "-0",
            "--checksum-seed=7",
            "--write-batch=bf",
            "--exclude=x",
            "-f",
            "P keep",
            "src/",
            "dst/",
        ],
        b"ARGV0 --filter='._-' '-a' '-0' --checksum-seed='7' --read-batch='bf' \
${1:-'dst/'} <<'#E#'\n- x\0-r keep\0;\n#E#\n",
    );
}

/// upstream: options.c:2280-2283 - `--dry-run` zeroes `write_batch` for both
/// write forms, so rsync 3.5.1 leaves neither the batch nor its `.sh` behind.
/// A batch written under `-n` would also be a lie: no update was applied, yet
/// the replay script claims to reproduce one.
#[test]
fn dry_run_write_batch_creates_no_files() {
    for option in ["--write-batch=bf", "--only-write-batch=bf"] {
        let args = ["-a", "-n", option, "src/", "dst/"];
        let (oc, upstream) = record_both(&args);
        assert!(oc.output.status.success(), "{option}: {:?}", oc.output);
        assert!(oc.batch.is_none(), "{option}: oc wrote a batch under -n");
        assert!(oc.script.is_none(), "{option}: oc wrote a .sh under -n");
        if let Some(up) = upstream {
            assert!(
                up.output.status.success(),
                "{option}: oracle {:?}",
                up.output
            );
            assert_eq!(
                (up.batch.is_some(), up.script.is_some()),
                (false, false),
                "{option}: oracle unexpectedly wrote batch files"
            );
        }
    }
}

/// Every implementation available to record or replay a cell: oc always,
/// the oracle when installed.
fn implementations() -> Vec<(&'static str, PathBuf)> {
    let mut all = vec![("oc", oc_rsync())];
    all.extend(upstream_rsync().map(|bin| ("upstream", bin)));
    all
}

/// upstream: batch.c:143-147 - the reconcile notice is `INFO_GTE(MISC, 1)`
/// output, so `--info=misc1` prints it without `-v` and `--info=misc0`
/// silences it under `-v`. A `-v`-only gate gets both wrong. (`-r` stays on
/// the replay: an inc-recursive batch refuses a replay without it,
/// compat.c:780-785.)
#[test]
fn read_batch_reconcile_notice_follows_info_misc() {
    const NOTICE: &str = "Setting the --checksum (-c) option to match the batchfile.";
    for (recorder, record_bin) in implementations() {
        let fx = Fixture::new();
        let out = fx.run(
            &record_bin,
            &["-r", "-c", "--write-batch=bf", "src/", "dst/"],
        );
        assert!(out.status.success(), "{recorder} record: {out:?}");
        for (replayer, replay_bin) in implementations() {
            for (args, printed) in [
                (["-r", "--info=misc1", "-n"].as_slice(), true),
                (["-r", "-v", "--info=misc0", "-n"].as_slice(), false),
            ] {
                let mut argv = vec!["--read-batch=bf"];
                argv.extend_from_slice(args);
                argv.push("dst/");
                let out = fx.run(&replay_bin, &argv);
                assert!(
                    out.status.success(),
                    "{recorder}->{replayer} {args:?}: {out:?}"
                );
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert_eq!(
                    stdout.lines().any(|line| line == NOTICE),
                    printed,
                    "{recorder}->{replayer} {args:?}: {stdout}"
                );
            }
        }
    }
}

/// upstream: main.c:1538-1546 - `--read-batch` checks only the destination
/// (`argv[argc-1]`) for a host spec; earlier operands are discarded, so a
/// remote-looking source is not an error.
#[test]
fn read_batch_ignores_remote_looking_source_operand() {
    for (recorder, record_bin) in implementations() {
        let fx = Fixture::new();
        let out = fx.run(&record_bin, &["-r", "--write-batch=bf", "src/", "dst/"]);
        assert!(out.status.success(), "{recorder} record: {out:?}");
        for (replayer, replay_bin) in implementations() {
            let dest = format!("{replayer}-replay/");
            let out = fx.run(
                &replay_bin,
                &["--read-batch=bf", "-r", "host::mod/src", &dest],
            );
            assert!(out.status.success(), "{recorder}->{replayer}: {out:?}");
            assert_eq!(
                fs::read(fx.root.join(&dest).join("a.txt")).unwrap(),
                b"alpha\n"
            );

            let out = fx.run(&replay_bin, &["--read-batch=bf", "-r", "host:dst/"]);
            assert_eq!(
                out.status.code(),
                Some(1),
                "{recorder}->{replayer}: {out:?}"
            );
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("remote destination is not allowed with --read-batch"),
                "{recorder}->{replayer}: {out:?}"
            );
        }
    }
}

/// upstream: options.c:1780-1792, 2265-2269 - the two write forms share one
/// `write_batch` mode, so combining them is legal and the later one decides;
/// only a write form with `--read-batch` is refused, with upstream's text.
#[test]
fn batch_option_combinations_match_upstream() {
    for (name, bin) in implementations() {
        // --only-write-batch last: no destination writes.
        let fx = Fixture::new();
        let out = fx.run(
            &bin,
            &[
                "-r",
                "--write-batch=bf",
                "--only-write-batch=bf",
                "src/",
                "dst/",
            ],
        );
        assert!(out.status.success(), "{name}: {out:?}");
        assert!(fx.root.join("bf").is_file(), "{name}: no batch");
        assert!(
            !fx.root.join("dst/a.txt").exists(),
            "{name}: only-write wrote dst"
        );

        // --write-batch last: the destination is updated too.
        let fx = Fixture::new();
        let out = fx.run(
            &bin,
            &[
                "-r",
                "--only-write-batch=bf",
                "--write-batch=bf",
                "src/",
                "dst/",
            ],
        );
        assert!(out.status.success(), "{name}: {out:?}");
        assert!(fx.root.join("bf").is_file(), "{name}: no batch");
        assert!(
            fx.root.join("dst/a.txt").is_file(),
            "{name}: write-batch skipped dst"
        );

        for write in ["--write-batch=x", "--only-write-batch=x"] {
            let fx = Fixture::new();
            let out = fx.run(&bin, &["-r", write, "--read-batch=bf", "dst/"]);
            assert_eq!(out.status.code(), Some(1), "{name} {write}: {out:?}");
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("--write-batch and --read-batch can not be used together"),
                "{name} {write}: {out:?}"
            );
        }
    }
}

/// Runs `args` with oc and the oracle from fresh fixtures prepared by
/// `prepare`, returning both recordings.
fn record_both_with(args: &[&OsStr], prepare: impl Fn(&Fixture)) -> (Recording, Option<Recording>) {
    let run = |bin: &Path| {
        let fx = Fixture::new();
        prepare(&fx);
        let output = Command::new(bin)
            .args(args)
            .current_dir(&fx.root)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        Recording {
            output,
            batch: fs::read(fx.root.join("bf")).ok(),
            script: fs::read(fx.root.join("bf.sh")).ok(),
        }
    };
    (run(&oc_rsync()), upstream_rsync().map(|bin| run(&bin)))
}

/// Asserts the masked `.sh` of each recording equals `pinned`.
fn assert_scripts(oc: &Recording, upstream: Option<&Recording>, pinned: &[u8], what: &str) {
    assert_eq!(
        oc.masked_script().as_deref(),
        Some(pinned),
        "{what}: oc .sh {:?} differs from rsync {ORACLE_VERSION}",
        oc.masked_script()
            .map(|s| String::from_utf8_lossy(&s).into_owned())
    );
    if let Some(up) = upstream {
        assert_eq!(
            up.masked_script().as_deref(),
            Some(pinned),
            "{what}: oracle drifted"
        );
    }
}

/// upstream: batch.c:164-198 write_arg() copies each `char *` argument
/// byte for byte, so a non-UTF-8 destination lands in `${1:-...}` unchanged.
/// A lossy conversion would write U+FFFD and the replay would target a
/// different directory.
#[test]
fn script_keeps_non_utf8_arguments_byte_exact() {
    use std::os::unix::ffi::OsStrExt;
    let dest = OsStr::from_bytes(b"d\xffst/");
    let args: [&OsStr; 4] = [
        "-r".as_ref(),
        "--write-batch=bf".as_ref(),
        "src/".as_ref(),
        dest,
    ];
    let (oc, upstream) = record_both_with(&args, |_| {});
    assert!(oc.output.status.success(), "{:?}", oc.output);
    assert_scripts(
        &oc,
        upstream.as_ref(),
        b"ARGV0 '-r' --read-batch='bf' ${1:-'d\xffst/'}\n",
        "non-UTF-8 destination",
    );
}

/// upstream: batch.c:305-311 elides `cooked_argv` - the operands as typed -
/// from the replay command. Under `--files-from` the source directory is one
/// of them even though the transfer list comes from the file, so it must not
/// survive into the script either.
#[test]
fn script_elides_operands_under_files_from() {
    let args: [&OsStr; 5] = [
        "-r".as_ref(),
        "--write-batch=bf".as_ref(),
        "--files-from=list".as_ref(),
        "src/".as_ref(),
        "dst/".as_ref(),
    ];
    let (oc, upstream) = record_both_with(&args, |fx| {
        fs::write(fx.root.join("list"), b"a.txt\n").unwrap();
    });
    assert!(oc.output.status.success(), "{:?}", oc.output);
    assert_scripts(
        &oc,
        upstream.as_ref(),
        b"ARGV0 '-r' --read-batch='bf' ${1:-'dst/'}\n",
        "--files-from",
    );
}

/// upstream: batch.c:247-258 opens the `.sh` at startup and compat.c:838-839
/// writes it during protocol setup, so a run that fails afterwards (here: the
/// destination's parent is a regular file) still leaves a complete script.
#[test]
fn script_survives_a_run_that_fails_after_setup() {
    let args: [&OsStr; 4] = [
        "-r".as_ref(),
        "--write-batch=bf".as_ref(),
        "src/".as_ref(),
        "file/sub/".as_ref(),
    ];
    let (oc, upstream) = record_both_with(&args, |fx| {
        fs::write(fx.root.join("file"), b"not a directory\n").unwrap();
    });
    assert!(!oc.output.status.success(), "the transfer itself must fail");
    assert_scripts(
        &oc,
        upstream.as_ref(),
        b"ARGV0 '-r' --read-batch='bf' ${1:-'file/sub/'}\n",
        "failed run",
    );
}

/// upstream: batch.c:255-258 - the `.sh` is created first and its open
/// failure exits RERR_FILESELECT (3) before any batch file exists.
#[test]
fn script_open_failure_exits_rerr_fileselect() {
    let args: [&OsStr; 4] = [
        "-r".as_ref(),
        "--write-batch=nodir/bf".as_ref(),
        "src/".as_ref(),
        "dst/".as_ref(),
    ];
    let (oc, upstream) = record_both_with(&args, |_| {});
    for (name, rec) in std::iter::once(("oc", &oc)).chain(upstream.iter().map(|r| ("upstream", r)))
    {
        assert_eq!(
            rec.output.status.code(),
            Some(3),
            "{name}: {:?}",
            rec.output
        );
        assert!(
            String::from_utf8_lossy(&rec.output.stderr).contains("bf.sh\" open error"),
            "{name}: {:?}",
            rec.output
        );
    }
}

/// upstream: batch.c:97-114 write_stream_flags() sets bit 9 (`tweaked_iconv`)
/// whenever `--iconv` is given, on a remote transfer as on a local one; a
/// replay of a batch whose header lacks it would decode the recorded names
/// without converting them. Pulled over a remote shell that runs the same
/// binary as its server.
#[test]
fn remote_batch_header_records_iconv() {
    let fx = Fixture::new();
    let shim = fx.root.join("rsh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    for (name, bin) in implementations() {
        let batch = format!("bf-{name}");
        let rsync_path = format!("--rsync-path={}", bin.display());
        let out = fx.run(
            &bin,
            &[
                "-r",
                "--iconv=UTF-8,ISO-8859-1",
                "-e",
                "./rsh",
                &rsync_path,
                &format!("--write-batch={batch}"),
                "localhost:src/",
                &format!("{name}-dst/"),
            ],
        );
        assert!(out.status.success(), "{name}: {out:?}");
        let header = fs::read(fx.root.join(&batch)).unwrap();
        // -r (bit 0), -d implied by -r (bit 7), --iconv (bit 9).
        assert_eq!(header[..4], 0x281i32.to_le_bytes(), "{name}: stream flags");
    }
}

/// upstream: batch.c:217-229 write_filter_rules() refuses a pattern holding
/// a newline only after writing the here-doc opener and the rules before it,
/// then exits RERR_SYNTAX; start_write_batch() never runs, so the batch file
/// holds just the stream flags written at open (main.c:1944). A NUL-separated
/// `--exclude-from` is how a newline reaches a pattern.
#[test]
fn newline_rule_leaves_upstreams_partial_script() {
    let args: [&OsStr; 6] = [
        "-r".as_ref(),
        "-0".as_ref(),
        "--write-batch=bf".as_ref(),
        "--exclude-from=rules".as_ref(),
        "src/".as_ref(),
        "dst/".as_ref(),
    ];
    let prepare = |fx: &Fixture| {
        fs::write(fx.root.join("rules"), b"- *.log\0- x\n#E#\ntouch PWNED\0").unwrap();
    };
    let (oc, upstream) = record_both_with(&args, prepare);
    let pinned: &[u8] =
        b"ARGV0 --filter='._-' '-r' '-0' --read-batch='bf' ${1:-'dst/'} <<'#E#'\n- *.log\0";
    for (name, rec) in std::iter::once(("oc", &oc)).chain(upstream.iter().map(|r| ("upstream", r)))
    {
        assert_eq!(
            rec.output.status.code(),
            Some(1),
            "{name}: {:?}",
            rec.output
        );
        assert!(
            String::from_utf8_lossy(&rec.output.stderr).contains(
                "cannot write a filter rule containing a newline to the batch replay script"
            ),
            "{name}: {:?}",
            rec.output
        );
        assert_eq!(
            rec.batch.as_ref().map(Vec::len),
            Some(4),
            "{name}: only the stream flags"
        );
    }
    assert_scripts(&oc, upstream.as_ref(), pinned, "newline rule");
}

/// upstream: batch.c:255-258 - the `.sh` open error names the file through
/// full_fname(), which prefixes a relative name with the working directory.
#[test]
fn script_open_error_names_the_full_path() {
    for (name, bin) in implementations() {
        let fx = Fixture::new();
        let out = fx.run(&bin, &["-r", "--write-batch=nodir/bf", "src/", "dst/"]);
        let expected = format!(
            "Batch file \"{}/nodir/bf.sh\" open error",
            fs::canonicalize(&fx.root).unwrap().display()
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&expected),
            "{name}: expected {expected:?} in {out:?}"
        );
    }
}

/// upstream: compat.c:739-740 sets CF_ID0_NAMES for a local transfer (its
/// child advertises 'u'), and with no INC_RECURSE (no `-r`) the header's
/// compat varint is `81 fe`.
/// The id lists then end with the name of id 0 (uidlist.c:398-405), so each
/// implementation must also replay the other's batch.
#[test]
fn local_batch_compat_flags_match_upstream() {
    let args = ["-lptgo", "--write-batch=bf", "src/a.txt", "dst/"];
    let (oc, upstream) = record_both(&args);
    assert!(oc.output.status.success(), "{:?}", oc.output);
    let oc_header = oc.batch.as_ref().expect("oc batch");
    assert_eq!(
        oc_header[4..10],
        [0x21, 0, 0, 0, 0x81, 0xfe],
        "oc protocol + compat"
    );
    if let Some(up) = upstream.as_ref() {
        assert_eq!(
            up.batch.as_ref().expect("oracle batch")[4..10],
            oc_header[4..10]
        );
    }

    for (recorder, record_bin) in implementations() {
        let fx = Fixture::new();
        let out = fx.run(&record_bin, &args);
        assert!(out.status.success(), "{recorder} record: {out:?}");
        for (replayer, replay_bin) in implementations() {
            let dest = format!("{replayer}-replay/");
            let out = fx.run(&replay_bin, &["--read-batch=bf", "-lptgo", &dest]);
            assert!(out.status.success(), "{recorder}->{replayer}: {out:?}");
            assert_eq!(
                fs::read(fx.root.join(&dest).join("a.txt")).unwrap(),
                b"alpha\n",
                "{recorder}->{replayer}"
            );
        }
    }
}
