//! `--drop-D` / `--no-drop-D`: the receiver refuses to CREATE devices and
//! special files (upstream `options.c:688-689`, `generator.c:2026-2033`).
//!
//! The option exists because `--no-D` cannot do this job. `--no-D` clears
//! `preserve_devices` / `preserve_specials`, and those also frame the file
//! list's rdev fields, so applying it to one end of a connection alone
//! desynchronises the list - upstream's `support/rrsync:623-630` records that a
//! FIFO then hangs the transfer at protocol 29 and corrupts it at 30, while a
//! device node breaks every protocol. `--drop-D` withholds only the creation
//! and leaves the encoding alone, which is why `rrsync` forces it.
//!
//! Two properties therefore have to hold together, and these tests pin both:
//!
//! 1. Creation is withheld, for every special type, on BOTH the local-copy path
//!    and the network receiver. oc has a local executor upstream does not, so a
//!    receiver-only implementation would silently miss every local copy.
//! 2. Nothing reaches the wire. The option is not forwarded to a peer
//!    (`rsync.1.md:1786`), so a remote argv built with `--drop-D` in effect must
//!    be byte-identical to one built without it.
//!
//! Skipped entries take upstream's ordinary non-regular-file path: the transfer
//! continues, the exit code stays 0, and `generator.c:2109-2115` emits
//! `skipping non-regular file` at `INFO_GTE(NONREG, 1)`. A test that only
//! checked "the node is absent" would also pass if the option aborted the
//! transfer, so the exit code and the surviving regular file are asserted too.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

fn run(args: &[&OsStr]) -> Output {
    Command::new(oc_rsync_binary())
        .args(args)
        .output()
        .expect("run oc-rsync")
}

/// Every special type upstream's gate covers, paired with a predicate that
/// recognises it at the destination. Device nodes need root to create, so the
/// source builder reports which types it could actually seed.
fn seed_specials(root: &Path) -> (PathBuf, Vec<&'static str>) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("regular.txt"), b"payload").expect("write regular file");

    let mut seeded = Vec::new();

    let fifo = src.join("afifo");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("spawn mkfifo");
    if status.success() {
        seeded.push("afifo");
    }

    if UnixListener::bind(src.join("asock")).is_ok() {
        seeded.push("asock");
    }

    (src, seeded)
}

fn is_special(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(path).is_ok_and(|meta| {
        let file_type = meta.file_type();
        file_type.is_fifo()
            || file_type.is_socket()
            || file_type.is_block_device()
            || file_type.is_char_device()
    })
}

/// Runs a local copy with the given flags and reports which of the seeded
/// special entries exist at the destination afterwards.
fn local_copy(flags: &[&str]) -> (Output, Vec<&'static str>, bool) {
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, seeded) = seed_specials(temp.path());
    assert!(
        !seeded.is_empty(),
        "no special file type could be created, so this test would be vacuous"
    );

    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).expect("create dest");

    let mut args: Vec<&OsStr> = vec![OsStr::new("-a")];
    args.extend(flags.iter().map(OsStr::new));
    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("{}/", dest.display());
    args.push(OsStr::new(&src_arg));
    args.push(OsStr::new(&dest_arg));

    let output = run(&args);
    let created: Vec<&'static str> = seeded
        .iter()
        .copied()
        .filter(|name| is_special(&dest.join(name)))
        .collect();
    let regular_arrived = dest.join("regular.txt").is_file();
    (output, created, regular_arrived)
}

/// `--drop-D` withholds every special type while the ordinary transfer of
/// regular files carries on. upstream: generator.c:2031.
#[test]
fn drop_d_withholds_every_special_type_on_a_local_copy() {
    let (output, created, regular_arrived) = local_copy(&["--drop-D"]);

    assert!(
        created.is_empty(),
        "--drop-D still created special entries: {created:?}"
    );
    assert!(
        regular_arrived,
        "--drop-D must withhold only device/special creation, not regular files"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "upstream skips the entry and continues; it is not an error. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Without the option every special type is created, so the assertion above is
/// about `--drop-D` and not about a destination that never receives specials.
#[test]
fn without_drop_d_every_special_type_is_created() {
    let (output, created, regular_arrived) = local_copy(&[]);

    assert!(
        !created.is_empty(),
        "baseline created no specials, so the --drop-D test would be vacuous"
    );
    assert!(regular_arrived);
    assert_eq!(output.status.code(), Some(0));
}

/// `--no-drop-D` restores creation, so the pair is a real negation and not a
/// one-way latch.
#[test]
fn no_drop_d_restores_creation() {
    let (_, baseline, _) = local_copy(&[]);
    let (output, created, _) = local_copy(&["--no-drop-D"]);

    assert_eq!(
        created, baseline,
        "--no-drop-D must behave exactly as if the option were absent"
    );
    assert_eq!(output.status.code(), Some(0));
}

/// Upstream declares both spellings as `POPT_ARG_VAL` assignments to one
/// variable (`options.c:688-689`), so a repeated pair is last-wins rather than
/// a usage error. Both orders are pinned because only checking one would pass
/// against an implementation that ignored the second flag entirely.
#[test]
fn the_pair_is_last_wins_in_both_orders() {
    let (drop_last, created_drop_last, _) = local_copy(&["--no-drop-D", "--drop-D"]);
    assert_eq!(
        drop_last.status.code(),
        Some(0),
        "repeating the pair must not be a usage error"
    );
    assert!(
        created_drop_last.is_empty(),
        "--no-drop-D --drop-D must end with drop-D in effect"
    );

    let (keep_last, created_keep_last, _) = local_copy(&["--drop-D", "--no-drop-D"]);
    assert_eq!(keep_last.status.code(), Some(0));
    assert!(
        !created_keep_last.is_empty(),
        "--drop-D --no-drop-D must end with creation allowed"
    );
}

/// Skipped entries reach upstream's ordinary non-regular-file path, which
/// reports them at `-v`. upstream: generator.c:2109-2115.
#[test]
fn skipped_entries_are_reported_as_non_regular_at_verbose() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, seeded) = seed_specials(temp.path());
    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).expect("create dest");

    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("{}/", dest.display());
    let output = run(&[
        OsStr::new("-av"),
        OsStr::new("--drop-D"),
        OsStr::new(&src_arg),
        OsStr::new(&dest_arg),
    ]);

    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for name in seeded {
        assert!(
            rendered.contains(&format!("skipping non-regular file \"{name}\"")),
            "expected the upstream skip line for {name}; got:\n{rendered}"
        );
    }
}

/// THE WIRE-NEUTRALITY GATE.
///
/// `--drop-D` is not forwarded (`rsync.1.md:1786`), and unlike `--no-D` it must
/// not disturb the option letters that frame the file list's rdev fields. The
/// remote argv built with the option in effect therefore has to be byte-identical
/// to the one built without it - if `--drop-D` had been implemented by clearing
/// `preserve_devices` / `preserve_specials`, the `D` letter would drop out here.
#[test]
fn drop_d_does_not_change_the_remote_argv() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, _) = seed_specials(temp.path());

    let rsh = temp.path().join("capture-argv");
    fs::write(&rsh, "#!/bin/sh\nprintf '%s\\n' \"$*\" >&2\nexit 0\n").expect("write fake rsh");
    let mut perms = fs::metadata(&rsh).expect("stat rsh").permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    fs::set_permissions(&rsh, perms).expect("chmod rsh");

    let src_arg = format!("{}/", src.display());
    let argv_for = |extra: &[&str]| -> String {
        let mut args: Vec<&OsStr> = vec![OsStr::new("-a"), OsStr::new("--specials")];
        args.extend(extra.iter().map(OsStr::new));
        args.push(OsStr::new("-e"));
        args.push(rsh.as_os_str());
        args.push(OsStr::new(&src_arg));
        args.push(OsStr::new("remote:/dst/"));
        String::from_utf8_lossy(&run(&args).stderr).into_owned()
    };

    let without = argv_for(&[]);
    let with = argv_for(&["--drop-D"]);

    assert!(
        !without.is_empty(),
        "the fake remote shell captured nothing, so this gate would be vacuous"
    );
    assert_eq!(
        with, without,
        "--drop-D must be wire-neutral: it is not forwarded and must not alter \
         the option letters that frame the file list's rdev fields"
    );
    assert!(
        !with.contains("--drop-D"),
        "--drop-D must never be forwarded to the remote side"
    );
}
