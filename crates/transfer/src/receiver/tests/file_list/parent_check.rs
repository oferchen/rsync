//! Without INC_RECURSE every entry's parent directory must be in the file
//! list, or the receiver aborts with exit 2 before generating anything.
//!
//! upstream: generator.c:1730-1752 recv_generator(). A sender that names
//! `dM/xyz` without sending `dM` is broken or hostile; rsync 3.5.2 prints
//! "ABORTING due to invalid path from sender: dM/xyz" and exits with
//! RERR_PROTOCOL. Without the check the receiver kept going, wrote what it
//! could and ended on a mkstemp failure (exit 23).

use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};

use protocol::filters::{FilterRuleWireFormat, RuleType};
use protocol::flist::{FileEntry, FileListWriter, sort_file_list};
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake_with_protocol};
use crate::config::ServerConfig;
use crate::receiver::dest_root::DestRoot;
use crate::writer::MsgInfoSender;

/// Collects the `MSG_ERROR` lines a server receiver sends its peer.
#[derive(Default)]
struct Errors(Vec<String>);

impl Write for Errors {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for Errors {
    fn send_msg_error(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.push(String::from_utf8_lossy(data).into_owned());
        Ok(())
    }
}

fn dir(name: &str) -> FileEntry {
    FileEntry::new_directory(PathBuf::from(name), 0o755)
}

fn file(name: &str) -> FileEntry {
    FileEntry::new_file(PathBuf::from(name), 1, 0o644)
}

fn missing_arg(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_file(PathBuf::from(name), 0, 0);
    entry.set_mode(0);
    entry
}

/// A receiver that read `entries` off the wire at `protocol`, as a sender
/// emits them, with `tweak` applied to its config first.
fn received(
    protocol: u8,
    entries: &[FileEntry],
    tweak: impl FnOnce(&mut ServerConfig),
) -> ReceiverContext {
    let version = ProtocolVersion::try_from(protocol).unwrap();
    let mut wire = Vec::new();
    let mut writer = FileListWriter::new(version);
    for entry in entries {
        let mut e = entry.clone();
        e.set_mtime(1_700_000_000, 0);
        writer.write_entry(&mut wire, &e).unwrap();
    }
    writer.write_end(&mut wire, None).unwrap();
    if protocol < 30 {
        // The sender's io_error int that follows the end marker below 30.
        wire.extend_from_slice(&0i32.to_le_bytes());
    }
    let mut config = test_config();
    config.protocol = version;
    tweak(&mut config);
    let mut ctx = ReceiverContext::new_for_test(&test_handshake_with_protocol(protocol), config);
    ctx.receive_file_list(&mut Cursor::new(wire)).unwrap();
    ctx
}

/// A receiver holding `entries` as its sorted list, for entries the wire
/// writer has no form for (mode-0 missing-arg sentinels).
fn listed(entries: Vec<FileEntry>, tweak: impl FnOnce(&mut ServerConfig)) -> ReceiverContext {
    let mut config = test_config();
    tweak(&mut config);
    let mut ctx = ReceiverContext::new_for_test(&test_handshake_with_protocol(32), config);
    let mut list = entries;
    sort_file_list(&mut list, false, false);
    ctx.file_list = list.into();
    ctx
}

fn check(ctx: &ReceiverContext) -> (io::Result<Vec<usize>>, Vec<String>) {
    check_into(ctx, Path::new("/nonexistent-dest"))
}

fn check_into(ctx: &ReceiverContext, dest: &Path) -> (io::Result<Vec<usize>>, Vec<String>) {
    let mut errors = Errors::default();
    let result = ctx.abort_on_entry_without_parent(dest, &mut errors);
    (result, errors.0)
}

fn abort_line<T: std::fmt::Debug>(result: io::Result<T>) -> String {
    let err = result.expect_err("the list names an entry whose parent was not sent");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let violation = err
        .get_ref()
        .and_then(|e| e.downcast_ref::<protocol::ProtocolViolation>())
        .expect("a protocol violation, which exits with RERR_PROTOCOL (2)");
    // generator.c:1750-1752: the generator raises it, so its trailer names
    // the generator role.
    assert!(violation.raised_by_generator());
    violation.cause_line().unwrap().to_owned()
}

/// The probe's list: `dM/xyz` arrives where `d/Mxyz` was sent, so `dM` is
/// never in the list.
fn orphan_list() -> Vec<FileEntry> {
    vec![
        dir("."),
        file("a1"),
        dir("d"),
        file("dM/xyz"),
        dir("zz"),
        file("zz/q"),
    ]
}

#[test]
fn an_entry_whose_parent_was_not_sent_aborts_with_upstreams_line() {
    for protocol in [28, 29, 30, 31, 32] {
        let ctx = received(protocol, &orphan_list(), |_| {});
        let (result, errors) = check(&ctx);
        assert_eq!(
            abort_line(result),
            "ABORTING due to invalid path from sender: dM/xyz",
            "protocol {protocol}"
        );
        assert!(errors.is_empty());
    }
}

/// WHY: the check must not over-refuse. Every list a real sender produces
/// names each parent, top-level entries have none, and "." is exempt
/// (generator.c:1733 `*dn != '.' || dn[1]`), including the implied
/// directories `-R` sends.
#[test]
fn lists_a_real_sender_produces_pass() {
    let lists: [Vec<FileEntry>; 4] = [
        vec![
            dir("."),
            file("a1"),
            dir("d"),
            file("d/Mxyz"),
            dir("d/e"),
            file("d/e/f"),
            dir("zz"),
            file("zz/q"),
        ],
        vec![file("a1"), file("b2")],
        vec![file("only")],
        vec![dir("d"), dir("d/e"), file("d/e/Mxyz")],
    ];
    for protocol in [28, 29, 32] {
        for list in &lists {
            let ctx = received(protocol, list, |_| {});
            let (result, errors) = check(&ctx);
            assert!(
                result.is_ok(),
                "protocol {protocol}: {list:?} refused: {result:?}"
            );
            assert!(errors.is_empty());
        }
    }
}

/// WHY: upstream finds the same-named entry and then requires it to be a
/// directory (`S_ISDIR(f->mode)`), so a file standing in for the parent is
/// still an invalid path.
#[test]
fn a_parent_that_is_not_a_directory_aborts() {
    let ctx = received(32, &[dir("."), file("x"), file("x/y")], |_| {});
    assert_eq!(
        abort_line(check(&ctx).0),
        "ABORTING due to invalid path from sender: x/y"
    );
}

/// WHY: the parent is looked up once per run of same-parent entries; a run
/// that starts valid must not hide a later run whose parent is missing.
#[test]
fn a_missing_parent_after_valid_runs_is_still_found() {
    let ctx = received(
        32,
        &[
            dir("."),
            dir("a"),
            file("a/1"),
            file("a/2"),
            dir("b"),
            file("b/1"),
            file("c/1"),
            file("c/2"),
        ],
        |_| {},
    );
    assert_eq!(
        abort_line(check(&ctx).0),
        "ABORTING due to invalid path from sender: c/1"
    );
}

/// WHY: under `--delete-missing-args` upstream warns for a missing-arg entry
/// (mode 0) whose parent is absent and keeps going (generator.c:1745-1746).
#[test]
fn a_missing_arg_without_its_parent_warns_and_continues() {
    let ctx = listed(
        vec![dir("."), dir("d"), file("d/f"), missing_arg("nope/gone")],
        |c| {
            c.file_selection.delete_missing_args = true;
        },
    );
    let (result, errors) = check(&ctx);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        errors,
        ["WARNING: parent dir is absent in the file list: nope\n"]
    );
}

/// WHY: a missing-arg parent counts as present for a missing-arg child
/// (generator.c:1740 `missing_args == 2 && !file->mode && !f->mode`).
#[test]
fn a_missing_arg_under_a_missing_arg_parent_passes_silently() {
    let ctx = listed(
        vec![dir("."), missing_arg("nope"), missing_arg("nope/gone")],
        |c| {
            c.file_selection.delete_missing_args = true;
        },
    );
    let (result, errors) = check(&ctx);
    assert!(result.is_ok(), "{result:?}");
    assert!(errors.is_empty());
}

/// WHY: the warning is for missing-arg entries only; a real entry without
/// its parent aborts even under `--delete-missing-args`, and without that
/// option a mode-0 entry is just another entry.
#[test]
fn only_a_missing_arg_under_delete_missing_args_escapes_the_abort() {
    let real = listed(vec![dir("."), file("nope/gone")], |c| {
        c.file_selection.delete_missing_args = true;
    });
    assert_eq!(
        abort_line(check(&real).0),
        "ABORTING due to invalid path from sender: nope/gone"
    );
    let without_option = listed(vec![dir("."), missing_arg("nope/gone")], |_| {});
    assert_eq!(
        abort_line(check(&without_option).0),
        "ABORTING due to invalid path from sender: nope/gone"
    );
}

/// WHY: upstream's walk returns before the check for `--list-only`
/// (generator.c:1675-1681) and for a dry run into a destination it did not
/// create (`dry_run > 1`, generator.c:1716); under INC_RECURSE the read-time
/// check (flist.c:3227-3245, exit 4) applies instead.
#[test]
fn list_only_dry_run_into_a_missing_destination_and_inc_recurse_skip_the_check() {
    let list_only = received(32, &orphan_list(), |c| c.flags.list_only = true);
    assert!(check(&list_only).0.is_ok());

    let mut dry = received(32, &orphan_list(), |_| {});
    dry.dest_root = DestRoot::DryRunAbsent;
    assert!(check(&dry).0.is_ok());

    let mut inc = received(32, &orphan_list(), |_| {});
    inc.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
    assert!(check(&inc).0.is_ok());
}

/// WHY: under `--iconv` the list stays in wire order (flist.c:3046-3047), so
/// the lookup must not assume the receiver's sort.
#[cfg(feature = "iconv")]
#[test]
fn iconv_keeps_wire_order_and_the_check_still_works() {
    let iconv = |c: &mut ServerConfig| {
        c.connection.iconv = Some(
            protocol::iconv::FilenameConverter::new("UTF-8", "ISO-8859-1")
                .expect("UTF-8/LATIN1 converter must construct on every platform"),
        );
    };
    // Directories ahead of the files that sort before them, as a sender
    // may emit them.
    let valid = [
        dir("."),
        dir("zz"),
        file("zz/q"),
        dir("d"),
        file("d/f"),
        file("a1"),
    ];
    let ctx = received(32, &valid, iconv);
    assert_eq!(
        ctx.file_list.live()[1].name(),
        "zz",
        "the list must stay in wire order"
    );
    assert!(check(&ctx).0.is_ok());

    let ctx = received(32, &orphan_list(), iconv);
    assert_eq!(
        abort_line(check(&ctx).0),
        "ABORTING due to invalid path from sender: dM/xyz"
    );
}

fn exclude(pattern: &str) -> FilterRuleWireFormat {
    FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: pattern.into(),
        ..FilterRuleWireFormat::default()
    }
}

/// WHY: upstream returns before the check for an entry the daemon's filter
/// refuses (generator.c:1698-1712) and for one below a listed directory it
/// refused (`skip_dir`, generator.c:1684-1695). A rule on the missing parent
/// alone does neither: that parent never reached recv_generator().
#[test]
fn daemon_filter_skips_exactly_what_upstream_skips() {
    let refused_entry = received(32, &orphan_list(), |c| {
        c.daemon_filter_rules = vec![exclude("xyz")]
    });
    assert!(check(&refused_entry).0.is_ok());

    let below_refused_dir = received(32, &[dir("."), dir("a"), file("a/b/c")], |c| {
        c.daemon_filter_rules = vec![exclude("a/")]
    });
    assert!(check(&below_refused_dir).0.is_ok());

    let rule_on_the_missing_parent = received(32, &orphan_list(), |c| {
        c.daemon_filter_rules = vec![exclude("dM/")]
    });
    assert_eq!(
        abort_line(check(&rule_on_the_missing_parent).0),
        "ABORTING due to invalid path from sender: dM/xyz"
    );
}

/// WHY: under `--dry-run` rsync 3.5.2 skips the check below a directory the
/// destination lacks (generator.c:1716 `dry_missing_dir`, set at :1883), so a
/// dry run onto an existing destination exits 0 there. Where that directory
/// exists the check still runs.
#[test]
fn a_dry_run_skips_entries_below_a_directory_the_destination_lacks() {
    let dest = tempfile::tempdir().expect("tempdir");
    let list = [dir("."), file("a1"), dir("d"), file("dM/xyz")];

    let dry = received(32, &list, |c| c.flags.dry_run = true);
    assert!(
        check_into(&dry, dest.path()).0.is_ok(),
        "`d` is missing, so `dM/xyz` is below it"
    );

    std::fs::create_dir(dest.path().join("d")).unwrap();
    assert_eq!(
        abort_line(check_into(&dry, dest.path()).0),
        "ABORTING due to invalid path from sender: dM/xyz"
    );

    let real = received(32, &list, |_| {});
    let empty = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        abort_line(check_into(&real, empty.path()).0),
        "ABORTING due to invalid path from sender: dM/xyz",
        "only a dry run skips"
    );
}

/// WHY: with implied directories missing (`-R --no-implied-dirs` below
/// protocol 30) `is_below()` also requires the name prefix (generator.c:1403),
/// so an entry deeper than a missing directory but not under it is checked.
#[test]
fn without_implied_dirs_a_dry_run_skips_only_entries_under_the_missing_directory() {
    let dest = tempfile::tempdir().expect("tempdir");
    let list = [dir("."), dir("a"), file("a/f"), file("bM/x")];
    let implied_missing = received(29, &list, |c| {
        c.flags.dry_run = true;
        c.flags.relative = true;
        c.flags.no_implied_dirs = true;
    });
    assert_eq!(
        abort_line(check_into(&implied_missing, dest.path()).0),
        "ABORTING due to invalid path from sender: bM/x"
    );
    let implied_sent = received(29, &list, |c| c.flags.dry_run = true);
    assert!(
        check_into(&implied_sent, dest.path()).0.is_ok(),
        "by depth alone `bM/x` is below the missing `a`"
    );
}

/// The probe's list below a listed directory: `d/M/yz` arrives where
/// `d/Mxyz` was sent, so `d/M` is missing but `d` is listed.
fn orphan_below_listed_dir() -> Vec<FileEntry> {
    vec![dir("."), dir("d"), file("d/M/yz"), dir("zz"), file("zz/q")]
}

fn index_of(ctx: &ReceiverContext, name: &str) -> usize {
    ctx.file_list
        .iter_indexed()
        .find(|(_, e)| e.path() == Path::new(name))
        .map(|(idx, _)| idx)
        .expect("listed")
}

/// WHY: whether rsync 3.5.2 aborts on `d/M/yz` depends on `d`: an entry below
/// a directory whose mkdir fails or that `--existing` skips returns before
/// the parent check (generator.c:1685-1695 `skip_dir`). While `d` is not yet
/// a directory at the destination, only the directory pass can tell, so the
/// entry is deferred; once `d` exists it can never be `skip_dir`, so the
/// check aborts at once.
#[test]
fn an_orphan_below_a_listed_directory_the_destination_lacks_is_deferred() {
    let dest = tempfile::tempdir().expect("tempdir");
    let ctx = received(32, &orphan_below_listed_dir(), |_| {});
    let (result, errors) = check_into(&ctx, dest.path());
    assert_eq!(result.unwrap(), [index_of(&ctx, "d/M/yz")]);
    assert!(errors.is_empty());

    std::fs::write(dest.path().join("d"), b"not a directory").unwrap();
    assert_eq!(
        check_into(&ctx, dest.path()).0.unwrap(),
        [index_of(&ctx, "d/M/yz")],
        "a non-directory in the way is replaced (or fails) at the walk"
    );

    std::fs::remove_file(dest.path().join("d")).unwrap();
    std::fs::create_dir(dest.path().join("d")).unwrap();
    assert_eq!(
        abort_line(check_into(&ctx, dest.path()).0),
        "ABORTING due to invalid path from sender: d/M/yz"
    );
}

/// WHY: a dry run never makes directories, so nothing is left for the walk to
/// decide; `dry_missing_dir` (generator.c:1883) already covers the missing
/// `d`, and an existing `d` aborts.
#[test]
fn a_dry_run_defers_nothing() {
    let dest = tempfile::tempdir().expect("tempdir");
    let dry = received(32, &orphan_below_listed_dir(), |c| c.flags.dry_run = true);
    assert!(check_into(&dry, dest.path()).0.unwrap().is_empty());
    std::fs::create_dir(dest.path().join("d")).unwrap();
    assert_eq!(
        abort_line(check_into(&dry, dest.path()).0),
        "ABORTING due to invalid path from sender: d/M/yz"
    );
}

/// WHY: at the walk the deferred entry is skipped exactly when `skip_dir`
/// covers it - a directory above it failed its mkdir (generator.c:1922) or
/// `--existing` found it missing (:1798) - and aborts with the same line
/// otherwise.
#[test]
fn a_deferred_orphan_is_skipped_only_below_a_skipped_directory() {
    use crate::receiver::directory::FailedDirectories;
    let ctx = received(32, &orphan_below_listed_dir(), |_| {});
    let idx = index_of(&ctx, "d/M/yz");

    let mut failed = FailedDirectories::new();
    failed.mark_failed("d");
    assert!(ctx.check_deferred_orphan(idx, &failed).is_ok());

    let mut existing = FailedDirectories::new();
    existing.mark_missing("d");
    assert!(ctx.check_deferred_orphan(idx, &existing).is_ok());

    let mut unrelated = FailedDirectories::new();
    unrelated.mark_failed("zz");
    unrelated.mark_missing("zz");
    assert_eq!(
        abort_line(ctx.check_deferred_orphan(idx, &unrelated)),
        "ABORTING due to invalid path from sender: d/M/yz"
    );
}
