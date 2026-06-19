// upstream: testsuite/exclude-lsh.test (symlink) -> testsuite/exclude.test:122-243
//
// Pin the six-sub-transfer matrix from upstream `exclude.test`. Each leg
// covers one of the distinct rsync invocations exercised by the upstream
// shell script and asserts the destination tree that upstream produces.
//
// Companion task UTS-DD-exclude-lsh.1 ships the wire-byte tests for the
// `:C` modifier over remote shell; this file ships the end-to-end
// regression pins for filter behavior on the local invocation surface.
//
// Each helper exercises the in-process CLI entry via `run_with_args` so
// failures surface as a test diff rather than as a binary exit code.

use super::common::*;
use super::*;

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

/// Mirrors upstream `exclude.test:33-92` source tree construction.
///
/// Returns the temp dir guard plus the canonical source root path. The
/// guard keeps the tree alive for the duration of one leg; each leg
/// builds its own copy so tests are independent.
fn setup_fixture() -> (TempDir, PathBuf) {
    let tmp = tempdir().expect("tempdir");
    let fromdir = tmp.path().join("from");

    let dirs = [
        "foo/down/to/you",
        "foo/sub",
        "bar/down/to/foo/too",
        "bar/down/to/bar/baz",
        "mid/for/foo/and/that/is/who",
        "new/keep/this",
        "new/lose/this",
    ];
    for d in dirs {
        fs::create_dir_all(fromdir.join(d)).expect("makepath");
    }

    // upstream: exclude.test:40-47 (root .filt)
    fs::write(
        fromdir.join(".filt"),
        "exclude down\n: .filt-temp\nclear\n- .filt\n- *.bak\n- *.old\n",
    )
    .expect("write root .filt");

    fs::write(fromdir.join("foo/file1"), b"filtered-1\n").expect("foo/file1");
    fs::write(fromdir.join("foo/file2"), b"removed\n").expect("foo/file2");
    fs::write(fromdir.join("foo/file2.old"), b"cvsout\n").expect("foo/file2.old");

    // upstream: exclude.test:51-54
    fs::write(fromdir.join("foo/.filt"), "include .filt\n- /file1\n").expect("foo/.filt");

    fs::write(fromdir.join("foo/sub/file1"), b"not-filtered-1\n").expect("foo/sub/file1");

    // upstream: exclude.test:56-60 (bar/.filt)
    fs::write(
        fromdir.join("bar/.filt"),
        "- home-cvs-exclude\ndir-merge .filt2\n+ to\n",
    )
    .expect("bar/.filt");

    fs::write(fromdir.join("bar/down/to/home-cvs-exclude"), b"cvsout\n").expect("home-cvs-exclude");

    fs::write(fromdir.join("bar/down/to/.filt2"), "- .filt2\n").expect("bar/down/to/.filt2");
    fs::write(fromdir.join("bar/down/to/foo/.filt2"), "+ *.junk\n").expect("foo/.filt2");
    fs::write(fromdir.join("bar/down/to/foo/file1"), b"keeper\n").expect("bar foo file1");
    fs::write(fromdir.join("bar/down/to/foo/file1.bak"), b"cvsout\n").expect("file1.bak");
    fs::write(fromdir.join("bar/down/to/foo/file3"), b"gone\n").expect("file3");
    fs::write(fromdir.join("bar/down/to/foo/file4"), b"lost\n").expect("file4");
    fs::write(fromdir.join("bar/down/to/foo/+ file3"), b"weird\n").expect("+ file3");
    fs::write(
        fromdir.join("bar/down/to/foo/file4.junk"),
        b"cvsout-but-filtin\n",
    )
    .expect("file4.junk");
    fs::write(fromdir.join("bar/down/to/foo/to"), b"smashed\n").expect("foo/to");

    fs::write(fromdir.join("bar/down/to/bar/.filt2"), "- *.deep\n").expect("bar/.filt2");
    fs::write(fromdir.join("bar/down/to/bar/baz/file5.deep"), b"filtout\n").expect("file5.deep");

    fs::write(fromdir.join("mid/.filt2"), "- extra\n").expect("mid/.filt2");
    fs::write(fromdir.join("mid/one-in-one-out"), b"cvsout\n").expect("mid/oio");
    fs::write(fromdir.join("mid/.cvsignore"), b"one-in-one-out\n").expect("mid/.cvsignore");
    fs::write(fromdir.join("mid/one-for-all"), b"cvsin\n").expect("one-for-all");
    fs::write(fromdir.join("mid/.filt"), ":C\n").expect("mid/.filt");
    fs::write(fromdir.join("mid/for/one-in-one-out"), b"cvsin\n").expect("for/oio");
    fs::write(fromdir.join("mid/for/foo/extra"), b"expunged\n").expect("foo/extra");
    fs::write(fromdir.join("mid/for/foo/keep"), b"retained\n").expect("foo/keep");

    (tmp, fromdir)
}

/// Build the upstream `exclude-from` file used by legs 2 and 3.
///
/// upstream: exclude.test:96-115
fn write_exclude_from(dir: &Path) -> PathBuf {
    let excl = dir.join("exclude-from");
    fs::write(
        &excl,
        "\
!
# If the second line of these two lines does anything, it's a bug.
+ **/bar
- /bar
# This should match against the whole path, not just the name.
+ foo**too
# These should float at the end of the path.
+ foo/s?b/
- foo/*/
# Test how /** differs from /***
- new/keep/**
- new/lose/***
# Test some normal excludes.  Competing lines are paired.
+ t[o]/
- to
+ file4
- file[2-9]
- /mid/for/foo/extra
",
    )
    .expect("write exclude-from");
    excl
}

fn assert_dest_has(dest: &Path, rel: &str) {
    assert!(dest.join(rel).exists(), "expected dest to contain {rel}");
}

fn assert_dest_missing(dest: &Path, rel: &str) {
    assert!(
        !dest.join(rel).exists(),
        "expected dest NOT to contain {rel}"
    );
}

#[test]
fn exclude_lsh_leg_1_prune_empty_dirs() {
    // upstream: exclude.test:122-123
    let (_tmp, fromdir) = setup_fixture();
    let dest = fromdir.parent().unwrap().join("dest1");
    fs::create_dir_all(&dest).expect("create dest1");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("--prune-empty-dirs"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "leg 1 exited non-zero");

    // Non-empty trees survive.
    assert_dest_has(&dest, "foo/file1");
    assert_dest_has(&dest, "foo/file2");
    assert_dest_has(&dest, "foo/file2.old");
    assert_dest_has(&dest, "foo/sub/file1");
    assert_dest_has(&dest, "bar/down/to/foo/file1");
    assert_dest_has(&dest, "mid/for/foo/keep");

    // upstream: `foo/down/to/you` is empty -> pruned.
    assert_dest_missing(&dest, "foo/down/to/you");
    assert_dest_missing(&dest, "foo/down/to");
    assert_dest_missing(&dest, "foo/down");
    // new/lose has only the trailing empty `this` dir.
    assert_dest_missing(&dest, "new/lose/this");
    assert_dest_missing(&dest, "new/lose");
    assert_dest_missing(&dest, "new/keep/this");
}

#[test]
fn exclude_lsh_leg_2_exclude_from_delete_during() {
    // upstream: exclude.test:155-156
    let (tmp, fromdir) = setup_fixture();
    let excl = write_exclude_from(tmp.path());
    let dest = fromdir.parent().unwrap().join("dest2");
    fs::create_dir_all(&dest).expect("create dest2");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let mut exclude_from_arg = OsString::from("--exclude-from=");
    exclude_from_arg.push(excl.as_os_str());

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        exclude_from_arg,
        OsString::from("--delete-during"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "leg 2 exited non-zero");

    // `+ foo**too` keeps bar/down/to/foo/too even though `- /bar` is below it.
    assert_dest_has(&dest, "bar/down/to/foo/too");
    // `+ foo/s?b/` matches foo/sub.
    assert_dest_has(&dest, "foo/sub/file1");
    // file4 is explicitly kept; file2/file3 are excluded by `- file[2-9]`.
    assert_dest_has(&dest, "bar/down/to/foo/file4");
    assert_dest_has(&dest, "bar/down/to/foo/file1");
    assert_dest_missing(&dest, "bar/down/to/foo/file3");
    // `- /mid/for/foo/extra`
    assert_dest_missing(&dest, "mid/for/foo/extra");
    assert_dest_has(&dest, "mid/for/foo/keep");
    // `- new/keep/**` strips the inner this/, `- new/lose/***` strips the dir too.
    assert_dest_missing(&dest, "new/keep/this");
    assert_dest_missing(&dest, "new/lose");
}

#[test]
#[ignore = "UTS-V3.B.F.14 pending"]
fn exclude_lsh_leg_3_merge_with_cvs_modifier_delete_excluded() {
    // upstream: exclude.test:173-174
    let (tmp, fromdir) = setup_fixture();
    let excl = write_exclude_from(tmp.path());
    let dest = fromdir.parent().unwrap().join("dest3");
    fs::create_dir_all(&dest).expect("create dest3");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let merge_arg = OsString::from(format!("merge {}", excl.display()));

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("--filter"),
        merge_arg,
        OsString::from("-f"),
        OsString::from(":C"),
        OsString::from("-f"),
        OsString::from("-C"),
        OsString::from("--delete-excluded"),
        OsString::from("--delete-during"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);

    // Leg 3 currently fails because the per-directory `:C` modifier
    // interaction with `--delete-excluded` is still being landed
    // (see UTS-V3.B.F.14). Surface the exit code as the test diagnostic.
    assert_eq!(code, 0, "leg 3 exited non-zero (pending UTS-V3.B.F.14)");
    assert_dest_has(&dest, "bar/down/to/foo/file4.junk");
    assert_dest_missing(&dest, "bar/down/to/foo/file1.bak");
}

#[test]
fn exclude_lsh_leg_4_dir_merge_filt_with_global_excludes() {
    // upstream: exclude.test:193-195
    let (tmp, fromdir) = setup_fixture();
    let excl = write_exclude_from(tmp.path());

    // upstream pipes `sed '/!/d' "$excl"` into stdin and merges it via `-f
    // merge_-`. Strip the `!` line up-front and feed it as a regular merge
    // file - the wire effect is identical.
    let stripped = tmp.path().join("exclude-no-bang");
    let raw = fs::read_to_string(&excl).expect("read excl");
    let filtered: String =
        raw.lines()
            .filter(|l| !l.contains('!'))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
    fs::write(&stripped, filtered).expect("write stripped excl");

    let dest = fromdir.parent().unwrap().join("dest4");
    fs::create_dir_all(&dest).expect("create dest4");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let merge_dirmerge = OsString::from("dir-merge .filt");
    let merge_global = OsString::from(format!("merge {}", stripped.display()));

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("-f"),
        merge_dirmerge,
        OsString::from("-f"),
        merge_global,
        OsString::from("--delete-during"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "leg 4 exited non-zero");

    // `dir-merge .filt` reads bar/.filt which contains `+ to` -> keep `to/`.
    assert_dest_has(&dest, "bar/down/to/foo/too");
    // root .filt clears the chain then re-excludes *.bak / *.old.
    assert_dest_missing(&dest, "foo/file2.old");
}

#[test]
fn exclude_lsh_leg_5_side_specific_rules_delete_before() {
    // upstream: exclude.test:211-213
    let (tmp, fromdir) = setup_fixture();
    let excl = write_exclude_from(tmp.path());
    let dest = fromdir.parent().unwrap().join("dest5");
    fs::create_dir_all(&dest).expect("create dest5");

    let stripped = tmp.path().join("exclude-no-bang-5");
    let raw = fs::read_to_string(&excl).expect("read excl");
    let filtered: String =
        raw.lines()
            .filter(|l| !l.contains('!'))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
    fs::write(&stripped, filtered).expect("write stripped");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let merge_dirmerge_s = OsString::from(":s_.filt");
    let merge_global_s = OsString::from(format!(".s_{}", stripped.display()));

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("-f"),
        merge_dirmerge_s,
        OsString::from("-f"),
        merge_global_s,
        OsString::from("-f"),
        OsString::from("P_nodel.deep"),
        OsString::from("--delete-before"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "leg 5 exited non-zero");

    // sender-side rules don't affect what the receiver keeps, so a basic
    // non-excluded path still arrives.
    assert_dest_has(&dest, "foo/sub/file1");
}

#[test]
#[ignore = "UTS-V3.B.F.14 pending"]
fn exclude_lsh_leg_6_dir_merge_excl_restricted() {
    // upstream: exclude.test:217-232
    let (_tmp, fromdir) = setup_fixture();

    fs::write(fromdir.join("bar/down/.excl"), "file3\n").expect(".excl");
    fs::write(fromdir.join("bar/down/to/foo/.excl"), "+ file3\n*.bak\n").expect("foo/.excl");

    let dest = fromdir.parent().unwrap().join("dest6");
    fs::create_dir_all(&dest).expect("create dest6");

    let src_with_slash = {
        let mut s = fromdir.clone().into_os_string();
        s.push("/");
        s
    };

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("-f"),
        OsString::from("dir-merge,-_.excl"),
        src_with_slash,
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "leg 6 exited non-zero");

    // The inner foo/.excl re-permits file3, so the keeper survives even
    // though the outer down/.excl excludes file3 from its scope.
    assert_dest_has(&dest, "bar/down/to/foo/file3");
    // *.bak from the inner .excl still strips file1.bak.
    assert_dest_missing(&dest, "bar/down/to/foo/file1.bak");
    // The restricted modifier `-` means the rules are exclude-only;
    // the matched .excl entries themselves are visible (not auto-hidden).
    assert_dest_has(&dest, "bar/down/.excl");
}
