//! Round-trip guard for the batch `.sh` replay wrapper: the option args oc
//! writes into `BATCH.sh`, when tokenized by the real POSIX shell exactly as a
//! `--read-batch` replay run tokenizes them, must decode back to the same
//! replay option set.
//!
//! `crates/batch/src/script.rs` emits the args one direction (write_arg-style
//! single-quoting, `--write-batch` -> `--read-batch`, filename elision, filter
//! stripping); #116/#117 pinned the emitted bytes and the stream-flags bitmap.
//! What was untested is the OTHER direction: feed the emitted `.sh` through
//! `/bin/sh` (its production consumer) and confirm the reconstructed argv is the
//! intended option set. A quoting or conversion regression that still produces
//! plausible bytes but tokenizes wrong would pass the byte-golden tests and fail
//! here.
//!
//! # Upstream Reference
//!
//! - `batch.c:255-312 write_batch_shell_file()` - the emit side this mirrors.
//! - `batch.c:164-190 write_arg()` - the single-quote wrapping being round-tripped.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use batch::script::generate_script_with_filters;
use batch::{BatchConfig, BatchMode};
use tempfile::tempdir;

/// Writes an executable shim that prints its argv NUL-separated. Used as the
/// batch invoker (`argv[0]`) so running the generated `.sh` under `/bin/sh`
/// captures exactly the argv the shell hands the replay binary after tokenizing
/// the emitted, quoted option string. `"$@"` excludes `$0`, so the captured
/// vector is precisely the replay options.
fn write_echo_shim(dir: &Path) -> PathBuf {
    let path = dir.join("echo_argv");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done\n",
    )
    .expect("write shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod shim");
    path
}

/// Runs the generated `.sh` under `/bin/sh` and returns the argv the shim
/// captured (the option tokens the shell parsed out of the emitted script).
fn replay_argv(script_path: &str) -> Vec<String> {
    let out = Command::new("/bin/sh")
        .arg(script_path)
        .output()
        .expect("run batch .sh");
    assert!(
        out.status.success(),
        "batch .sh must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// The full emit->shell-parse round-trip: pass-through flags survive verbatim
/// (including special-char values that must be quoted), `--write-batch` becomes
/// `--read-batch` with the same batch name, filter/exclude options are stripped
/// (replayed via the heredoc), filename operands are elided, and the `${1:-dest}`
/// default supplies the destination.
#[test]
fn batch_sh_options_round_trip_through_the_shell_to_the_replay_set() {
    let dir = tempdir().expect("tempdir");
    let shim = write_echo_shim(dir.path());
    let batch = dir.path().join("mybatch");
    let batch_str = batch.to_string_lossy().into_owned();

    // A value carrying shell specials that force single-quote wrapping: a space,
    // a `$`, a `*` and a `;`. Single-quoting protects all of them, so they must
    // survive the shell tokenization byte-for-byte.
    let partial = "--partial-dir=.rsync tmp$*;x";

    let config = BatchConfig::new(BatchMode::Write, batch_str.clone(), 31)
        .with_invoker(shim.to_string_lossy().into_owned())
        .with_replay_args([
            "oc-rsync".to_string(),
            "-a".to_string(),
            "--numeric-ids".to_string(),
            partial.to_string(),
            "--exclude=*.tmp".to_string(),
            format!("--write-batch={batch_str}"),
            "srcfile".to_string(),
            "destdir".to_string(),
        ])
        .with_operands(["srcfile".to_string(), "destdir".to_string()]);

    generate_script_with_filters(&config, Some("+ keep\n- *\n"), Some("destdir"))
        .expect("generate .sh");

    let argv = replay_argv(&config.script_file_path());

    // Transfer flags pass through verbatim.
    assert!(argv.iter().any(|a| a == "-a"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "--numeric-ids"), "{argv:?}");
    // The special-char value round-trips exactly through the quoting.
    assert!(
        argv.iter().any(|a| a == partial),
        "special-char option value must survive quoting: {argv:?}"
    );
    // --write-batch is converted to --read-batch with the same batch name.
    assert!(
        argv.iter()
            .any(|a| a == &format!("--read-batch={batch_str}")),
        "write-batch must convert to read-batch: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.starts_with("--write-batch")),
        "no --write-batch may survive: {argv:?}"
    );
    // Filters are replayed via the heredoc, so the filter option is injected once
    // and the original --exclude is dropped.
    assert!(
        argv.iter().any(|a| a == "--filter=._-"),
        "heredoc filter option must be present: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.starts_with("--exclude")),
        "--exclude is replayed via the heredoc, not the argv: {argv:?}"
    );
    // Filename operands are elided; the only positional is the ${1:-destdir}
    // default (last token), and the source operand never reappears.
    assert!(
        !argv.iter().any(|a| a == "srcfile"),
        "source operand must be elided: {argv:?}"
    );
    assert_eq!(
        argv.last().map(String::as_str),
        Some("destdir"),
        "the ${{1:-dest}} default supplies the destination: {argv:?}"
    );
}

/// An arg value containing a literal single quote round-trips through the
/// `.sh` intact.
///
/// This was a KNOWN LIMITATION until upstream 3.5.0: `write_arg()` used to wrap
/// in single quotes and emit an embedded `'` as `''`, which POSIX sh collapses
/// to nothing (`'a''b'` tokenizes to `ab`), silently corrupting the option
/// value in the generated replay script. 3.5.0 switched to the POSIX
/// close/escape/reopen idiom `'\''` (batch.c:189-192) and the loss is gone.
///
/// Pinned in the round-trip direction - through the real `/bin/sh` - because
/// the byte-golden tests in `script.rs` cannot tell a quoting form that *looks*
/// right from one that *tokenizes* right.
#[test]
fn embedded_single_quote_survives_the_sh_round_trip() {
    let dir = tempdir().expect("tempdir");
    let shim = write_echo_shim(dir.path());
    let batch = dir.path().join("b");
    let batch_str = batch.to_string_lossy().into_owned();

    let config = BatchConfig::new(BatchMode::Write, batch_str.clone(), 31)
        .with_invoker(shim.to_string_lossy().into_owned())
        .with_replay_args([
            "oc-rsync".to_string(),
            "--suffix=a'b".to_string(),
            format!("--write-batch={batch_str}"),
            "dst".to_string(),
        ])
        .with_operands(["dst".to_string()]);

    generate_script_with_filters(&config, None, Some("dst")).expect("generate .sh");

    let argv = replay_argv(&config.script_file_path());

    // upstream 3.5.0 emits the POSIX close/escape/reopen idiom (`'a'\''b'`,
    // batch.c:189-192), so the quote survives evaluation. Its 3.4.x predecessor
    // wrote `'a''b'`, which the shell collapses to `ab` - the option value was
    // silently corrupted by the generated replay script.
    assert!(
        argv.iter().any(|a| a == "--suffix=a'b"),
        "the literal single quote must survive the .sh round-trip: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--suffix=ab"),
        "the quote must not be collapsed away (the pre-3.5.0 behaviour): {argv:?}"
    );
}

/// Shell-hostile values: each one, if it reached `/bin/sh` unquoted, would run
/// a command, redirect into a file, or split into several argv elements. Every
/// side effect targets the file `CANARY` in the script's working directory.
const HOSTILE: &[&str] = &[
    "d`touch CANARY`x/",
    "$(touch CANARY)",
    "a;touch CANARY",
    "a>CANARY",
    "a<missing",
    "say \"hi\"",
    "it's",
    "line1\nline2",
    "sp ace",
];

/// Filter rules with shell metacharacters. The quoted `<<'#E#'` delimiter
/// disables expansion inside the here-doc, so these must reach stdin verbatim.
const HOSTILE_RULES: &str = "- `touch CANARY`\n- $(touch CANARY);x>CANARY\n+ it's \"q\"\n";

/// Mirrors upstream `write_arg()` (batch.c:164-196) from the C source: a plain
/// `-opt=` prefix stays bare, everything after it is single-quoted, and an
/// embedded `'` becomes `'\''`.
fn upstream_write_arg(arg: &str) -> String {
    let mut out = String::new();
    let mut rest = arg;
    if let Some(x) = arg.find('=')
        && arg.starts_with('-')
        && arg[..x]
            .bytes()
            .all(|c| c == b'-' || c == b'_' || c.is_ascii_alphanumeric())
    {
        out.push_str(&arg[..=x]);
        rest = &arg[x + 1..];
    }
    out.push('\'');
    out.push_str(&rest.replace('\'', r"'\''"));
    out.push('\'');
    out
}

/// Writes a shim that prints its argv NUL-separated and copies stdin (the
/// filter here-doc) to `stdin.out` next to itself.
fn write_argv_and_stdin_shim(dir: &Path) -> PathBuf {
    let path = dir.join("argv_stdin");
    let body = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done\ncat > '{}'\n",
        dir.join("stdin.out").display()
    );
    std::fs::write(&path, body).expect("write shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod shim");
    path
}

/// Builds a `--write-batch` config whose pass-through args are every
/// [`HOSTILE`] value (bare and as an `--suffix=` value) and whose destination
/// operand is `dest`.
fn hostile_config(dir: &Path, shim: &Path, dest: &str) -> BatchConfig {
    let batch_str = dir.join("b").to_string_lossy().into_owned();
    let mut args = vec!["oc-rsync".to_string()];
    args.extend(HOSTILE.iter().map(|s| (*s).to_string()));
    args.push("--suffix=`touch CANARY`".to_string());
    args.push(format!("--write-batch={batch_str}"));
    args.push("src".to_string());
    args.push(dest.to_string());
    BatchConfig::new(BatchMode::Write, batch_str, 31)
        .with_invoker(shim.to_string_lossy().into_owned())
        .with_replay_args(args)
        .with_operands(["src".to_string(), dest.to_string()])
}

/// Every hostile argument and destination must reach the replay binary as ONE
/// literal argv element, the here-doc rules must reach stdin verbatim, and no
/// command may run.
///
/// The generated `BATCH.sh` is executed by `/bin/sh` when the operator replays
/// the batch, and its values come from the original command line, which can
/// carry attacker-chosen names (a destination taken from a hostile listing, a
/// filter rule from a dir-merge file). Before upstream 3.5.0 made quoting
/// unconditional, a destination like ``d`touch CANARY`x/`` was written bare
/// and the backtick executed on replay.
///
/// upstream: batch.c:164-196 write_arg(), batch.c:213-240 write_filter_rules().
#[test]
fn hostile_values_round_trip_through_sh_as_single_literal_args() {
    for dest in HOSTILE {
        let dir = tempdir().expect("tempdir");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).expect("mkdir work");
        let shim = write_argv_and_stdin_shim(dir.path());
        let config = hostile_config(dir.path(), &shim, dest);

        generate_script_with_filters(&config, Some(HOSTILE_RULES), Some(dest))
            .expect("generate .sh");

        let out = Command::new("/bin/sh")
            .arg(config.script_file_path())
            .current_dir(&work)
            .output()
            .expect("run batch .sh");

        assert!(
            !work.join("CANARY").exists(),
            "dest {dest:?}: the replay script executed an injected command"
        );
        assert!(
            out.status.success(),
            "dest {dest:?}: batch .sh must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let argv: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let mut expected = vec!["--filter=._-".to_string()];
        expected.extend(HOSTILE.iter().map(|s| (*s).to_string()));
        expected.push("--suffix=`touch CANARY`".to_string());
        expected.push(format!("--read-batch={}", config.batch_path));
        expected.push((*dest).to_string());
        assert_eq!(
            argv, expected,
            "dest {dest:?}: argv must round-trip exactly"
        );

        let stdin = std::fs::read_to_string(dir.path().join("stdin.out")).expect("stdin capture");
        assert_eq!(
            stdin, HOSTILE_RULES,
            "filter rules must reach the replay binary verbatim"
        );
    }
}

/// The emitted script must be byte-identical to what upstream
/// `write_batch_shell_file()` writes for the same command line.
///
/// The first case is transcribed from the real rsync 3.5.0 binary
/// (`rsync -a --exclude='*.tmp' --suffix="a'b" --write-batch=B src/
/// 'd`touch PWN`x/'`), with only the invoker path substituted:
///
/// ```text
/// '<rsync>' --filter='._-' '-a' --suffix='a'\''b' --read-batch='B' ${1:-'d`touch PWN`x/'} <<'#E#'
/// - *.tmp
/// #E#
/// ```
///
/// Note that upstream quotes even the `._-` value of the injected `--filter`
/// option, because `write_opt()` routes it through `write_arg()`.
#[test]
fn emitted_script_is_byte_identical_to_upstream() {
    let dir = tempdir().expect("tempdir");
    let batch_str = dir.path().join("B").to_string_lossy().into_owned();

    let config = BatchConfig::new(BatchMode::Write, batch_str.clone(), 31)
        .with_invoker("/usr/bin/rsync")
        .with_replay_args([
            "rsync".to_string(),
            "-a".to_string(),
            "--exclude=*.tmp".to_string(),
            "--suffix=a'b".to_string(),
            format!("--write-batch={batch_str}"),
            "src/".to_string(),
            "d`touch PWN`x/".to_string(),
        ])
        .with_operands(["src/".to_string(), "d`touch PWN`x/".to_string()]);
    generate_script_with_filters(&config, Some("- *.tmp\n"), Some("d`touch PWN`x/"))
        .expect("generate .sh");
    let content = std::fs::read_to_string(config.script_file_path()).expect("read .sh");
    assert_eq!(
        content,
        format!(
            "'/usr/bin/rsync' --filter='._-' '-a' --suffix='a'\\''b' --read-batch='{batch_str}' \
             ${{1:-'d`touch PWN`x/'}} <<'#E#'\n- *.tmp\n#E#\n"
        )
    );

    // The full hostile set, against the write_arg() transcription above.
    let hostile = tempdir().expect("tempdir");
    let dest = "d`touch CANARY`x/";
    let config = hostile_config(hostile.path(), Path::new("/usr/bin/rsync"), dest);
    generate_script_with_filters(&config, Some(HOSTILE_RULES), Some(dest)).expect("generate .sh");
    let content = std::fs::read_to_string(config.script_file_path()).expect("read .sh");

    let mut expected = upstream_write_arg("/usr/bin/rsync");
    expected.push_str(" --filter=");
    expected.push_str(&upstream_write_arg("._-"));
    for arg in HOSTILE
        .iter()
        .copied()
        .chain(std::iter::once("--suffix=`touch CANARY`"))
    {
        expected.push(' ');
        expected.push_str(&upstream_write_arg(arg));
    }
    expected.push_str(" --read-batch=");
    expected.push_str(&upstream_write_arg(&config.batch_path));
    expected.push_str(" ${1:-");
    expected.push_str(&upstream_write_arg(dest));
    expected.push_str("} <<'#E#'\n");
    expected.push_str(HOSTILE_RULES);
    expected.push_str("#E#\n");
    assert_eq!(content, expected);
}
