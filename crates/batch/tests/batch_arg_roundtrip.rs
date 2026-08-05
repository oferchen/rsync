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
        argv.iter().any(|a| a == &format!("--read-batch={batch_str}")),
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

/// KNOWN LIMITATION (shared with upstream, pinned not papered over): an arg
/// value containing a literal single quote does NOT round-trip through the
/// `.sh`. Upstream `write_arg()` (batch.c:174-185) wraps in single quotes and
/// emits an embedded `'` as `''`, which POSIX sh collapses to nothing
/// (`'a''b'` tokenizes to `ab`). oc mirrors that byte-for-byte for `.sh`
/// fidelity (#116), so it inherits the same lossy behavior - this is NOT an
/// oc-vs-upstream divergence. Pinned so any change to the quoting is a
/// deliberate, reviewed decision.
#[test]
fn embedded_single_quote_is_lossy_matching_upstream_write_arg() {
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

    // The single quote is dropped by the shell (`'a''b'` -> `ab`), exactly as an
    // upstream-generated batch `.sh` would behave.
    assert!(
        argv.iter().any(|a| a == "--suffix=ab"),
        "embedded single quote is collapsed (upstream-identical): {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--suffix=a'b"),
        "the literal single quote does not survive the .sh round-trip: {argv:?}"
    );
}
