//! A pulling client sends an itemize record for every entry with significant
//! flags, as rsync 3.5.2's generator does whichever side it runs on.
//!
//! upstream: generator.c:554 `itemize()` writes `write_ndx(sock_f_out, ndx)`
//! and the `iflags` shortint for every entry whose flags are significant
//! (generator.c:625-630): a directory whose time changes, a new symlink, a
//! file whose mode alone differs. Nothing there asks which role the process
//! has. The sender logs each record and echoes it (sender.c:589-591), and the
//! echoes are part of the stream `--write-batch` records on a pull.
//!
//! An oc client receiver sent the transfer requests only. Its own output was
//! right, because it prints its rows itself, but the server's log held no row
//! for a directory, a symlink or an attribute-only change, and a batch written
//! on a pull replayed without them.
//!
//! Every cell compares an oc client with a 3.5.2 client against the same
//! 3.5.2 server.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::FileTime;
use test_support::wire_records::{
    ITEM_TRANSFER, Record, demultiplex, generator_records, skip_filter_list,
};
use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);
const SOURCE_MTIME: i64 = 1_577_934_245;
const DEST_MTIME: i64 = 1_546_300_800;

fn set_mtime(path: &Path, seconds: i64) {
    let time = FileTime::from_unix_time(seconds, 0);
    test_support::file_times::set_symlink_file_times(path, time, time).expect("set mtime");
}

/// `src/` holds `a`, `l -> a`, `p`, `u`, the FIFO `fifo`, the hard-linked
/// pair `h1` and `h2`, `newdir/n` and `sub/f`.
fn build_source(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).expect("create src/sub");
    fs::create_dir(src.join("newdir")).expect("create src/newdir");
    fs::write(src.join("a"), "a-new\n").expect("write a");
    fs::write(src.join("p"), "same\n").expect("write p");
    fs::write(src.join("u"), "same\n").expect("write u");
    fs::write(src.join("newdir/n"), "n\n").expect("write newdir/n");
    fs::write(src.join("sub/f"), "f\n").expect("write sub/f");
    symlink("a", src.join("l")).expect("symlink l");
    fs::write(src.join("h1"), "h\n").expect("write h1");
    fs::hard_link(src.join("h1"), src.join("h2")).expect("link h2");
    let made = Command::new("mkfifo")
        .arg(src.join("fifo"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    for name in [
        "newdir/n", "newdir", "sub/f", "sub", "a", "l", "p", "u", "h1", "fifo", "",
    ] {
        set_mtime(&src.join(name), SOURCE_MTIME);
    }
    write_remote_shell(root);
}

/// A remote shell that runs the server locally and copies everything the
/// client sends it into the file named by `CAP`. The copier inherits the
/// client's stderr, so a run is not over until its capture is complete.
fn write_remote_shell(root: &Path) {
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/bash\nshift\nexec \"$@\" < <(tee \"$CAP\")\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
}

/// The destination a run starts from: the root and `sub/` differ in time
/// only, `a` is older and of another size, `p` differs in mode only, `u` has
/// the same content and an older time, and `l`, `fifo`, `h2`, `newdir/` and
/// `sub/f` are absent. `h1` is up to date, so under `-H` its follower `h2` is linked and
/// not transferred.
fn build_destination(dst: &Path) {
    fs::create_dir_all(dst.join("sub")).expect("create dst");
    fs::write(dst.join("a"), "a-old-longer\n").expect("write old a");
    fs::write(dst.join("p"), "same\n").expect("write p");
    fs::write(dst.join("u"), "same\n").expect("write u");
    fs::write(dst.join("h1"), "h\n").expect("write h1");
    fs::set_permissions(dst.join("p"), fs::Permissions::from_mode(0o600)).expect("chmod p");
    set_mtime(&dst.join("p"), SOURCE_MTIME);
    set_mtime(&dst.join("h1"), SOURCE_MTIME);
    for name in ["u", "sub", "a", ""] {
        set_mtime(&dst.join(name), DEST_MTIME);
    }
}

fn run(command: &mut Command, what: &str) -> (Option<i32>, String, String) {
    match run_deadlined(command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => (
            status.code(),
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        ),
        Deadlined::Expired { stdout, stderr, .. } => panic!(
            "{what} did not finish\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ),
    }
}

/// What one pull printed, what it sent, what the server logged for it, and
/// the batch it wrote.
struct Pull {
    stdout: String,
    /// Everything the client sent, as captured by the remote shell.
    sent: Vec<u8>,
    server_rows: Vec<String>,
    batch: PathBuf,
    /// `None` when the run wrote no batch, as a dry run does not.
    batch_len: Option<u64>,
}

impl Pull {
    /// The generator records in what the client sent.
    fn records(&self) -> Vec<Record> {
        generator_records(skip_filter_list(&demultiplex(&self.sent)))
    }
}

/// Whether `server` is this binary, which takes no `--log-file-format`.
fn is_this_binary(server: &Path) -> bool {
    server == Path::new(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Pulls `src/` with `client` from `server`. The server logs each item it
/// handles as `%i %n%L`, which is one row per record the client sent it.
fn pull(root: &Path, client: &Path, server: &Path, flags: &[&str], tag: &str) -> Pull {
    pull_into(root, client, server, flags, tag, build_destination)
}

/// [`pull`] into a destination prepared by `prepare`.
fn pull_into(
    root: &Path,
    client: &Path,
    server: &Path,
    flags: &[&str],
    tag: &str,
    prepare: fn(&Path),
) -> Pull {
    let dst = root.join(format!("dst-{tag}"));
    prepare(&dst);
    let capture = root.join(format!("sent-{tag}"));
    let log = root.join(format!("server-{tag}.log"));
    let batch = root.join(format!("batch-{tag}"));
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .env("CAP", &capture)
        .args(flags)
        .arg("--checksum-seed=1")
        .arg(format!("--write-batch={}", batch.display()));
    let logged = !is_this_binary(server);
    if logged {
        command
            .arg(format!("-M--log-file={}", log.display()))
            .arg("-M--log-file-format=%i %n%L");
    }
    command
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .arg(format!("localhost:{}/", root.join("src").display()))
        .arg(format!("{}/", dst.display()));
    let (status, stdout, stderr) = run(&mut command, tag);
    assert_eq!(status, Some(0), "{tag}: {stderr}");
    // Each log line is `date time [pid] text`; the closing line carries byte
    // counts, which also cover the handshake and are not compared.
    let server_rows = if logged {
        fs::read_to_string(&log).expect("server log")
    } else {
        String::new()
    };
    let server_rows = server_rows
        .lines()
        .filter_map(|line| line.split_once("] ").map(|(_, text)| text.to_owned()))
        .filter(|text| !text.starts_with("sent "))
        .collect();
    let batch_len = fs::metadata(&batch).ok().map(|meta| meta.len());
    let sent = fs::read(&capture).expect("captured stream");
    Pull {
        stdout,
        sent,
        server_rows,
        batch,
        batch_len,
    }
}

/// Replays `batch` with `binary` onto a fresh copy of the starting
/// destination and returns the rows it printed.
fn replay(root: &Path, binary: &Path, batch: &Path, flags: &[&str], tag: &str) -> String {
    let dst = root.join(format!("replay-{tag}"));
    build_destination(&dst);
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .args(flags)
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", dst.display()));
    let (status, stdout, stderr) = run(&mut command, tag);
    assert_eq!(status, Some(0), "{tag}: {stderr}");
    // The `-v` summary counts handshake bytes, which are not compared.
    stdout
        .lines()
        .filter(|line| !line.starts_with("sent "))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn compare_with_upstream(flags: &[&str]) {
    compare(flags, true);
}

/// [`compare_with_upstream`] without the client's own output and without a
/// replay by this binary, for flag sets whose printed lines differ from 3.5.2
/// for reasons outside these records.
fn compare_records_with_upstream(flags: &[&str]) {
    compare(flags, false);
}

#[derive(Clone, Copy, PartialEq)]
enum Rows {
    /// The server logs the same rows in the same order.
    InOrder,
    /// The server logs the same rows; their order is not compared.
    AnyOrder,
}

fn compare(flags: &[&str], client_output: bool) {
    compare_rows(flags, client_output, Rows::InOrder);
}

fn compare_rows(flags: &[&str], client_output: bool, rows: Rows) {
    compare_from(Some(UpstreamVersion::V3_5_2), flags, client_output, rows);
}

/// Pulls from `server` - an upstream release, or this binary for `None` -
/// with a 3.5.2 client and with this client, and compares the two runs.
fn compare_from(server: Option<UpstreamVersion>, flags: &[&str], client_output: bool, rows: Rows) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let served;
    let server = match server {
        Some(UpstreamVersion::V3_5_2) => up,
        Some(version) => {
            let Some(found) = require_upstream_rsync(version) else {
                return;
            };
            served = found;
            served.binary()
        }
        None => oc.as_path(),
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_source(&root);

    let reference = pull(&root, up, server, flags, "up");
    if flags.contains(&"-aiH") {
        assert!(
            reference
                .server_rows
                .iter()
                .any(|logged| logged == "hf+++++++++\\ h2 => h1"),
            "the server must log the linked follower for 3.5.2 {flags:?}: {:#?}",
            reference.server_rows
        );
    }
    // The oracle must log entries that are not transferred, or agreeing with
    // it says nothing about their records.
    let must_log: &[&str] = if is_this_binary(server) {
        &[]
    } else {
        &[
            ".d..t......\\ ./",
            "cL+++++++++\\ l -> a",
            ".f...p.....\\ p",
            "cS+++++++++\\ fifo",
            "cd+++++++++\\ newdir/",
            ".d..t......\\ sub/",
        ]
    };
    for row in must_log {
        assert!(
            reference.server_rows.iter().any(|logged| logged == row),
            "the server must log {row:?} for 3.5.2 {flags:?}: {:#?}",
            reference.server_rows
        );
    }
    let mut reference = reference;
    let mut ours = pull(&root, &oc, server, flags, "oc");
    if rows == Rows::AnyOrder {
        reference.server_rows.sort();
        ours.server_rows.sort();
    }

    // At protocol 29 a client does not multiplex what it sends, so there the
    // server's log stands for the records.
    if !flags.contains(&"--protocol=29") {
        let (mut sent, mut expected) = (ours.records(), reference.records());
        if rows == Rows::AnyOrder {
            sent.sort();
            expected.sort();
        }
        assert!(
            expected
                .iter()
                .any(|(_, iflags, _)| iflags & ITEM_TRANSFER == 0),
            "3.5.2 {flags:?} must send records that request no transfer"
        );
        // upstream: generator.c:2135-2136 - a new FIFO carries
        // ITEM_LOCAL_CHANGE | ITEM_IS_NEW | ITEM_REPORT_CHANGE.
        assert!(
            expected.iter().any(|(_, iflags, _)| *iflags == 0x6002),
            "3.5.2 {flags:?} must itemize the new FIFO with 0x6002: {expected:x?}"
        );
        assert_eq!(sent, expected, "{flags:?}: the records the client sent");
    }
    assert_eq!(
        ours.server_rows, reference.server_rows,
        "{flags:?}: what the server logged"
    );
    if client_output {
        assert_eq!(ours.stdout, reference.stdout, "{flags:?}: client output");
    }
    // The batch is the server's stream, echoes included, so with the same
    // sender its length differs by exactly the records the client left out.
    // A record sent out of place costs more index bytes, so the length is
    // compared only where the order is.
    if rows == Rows::InOrder {
        assert_eq!(
            ours.batch_len, reference.batch_len,
            "{flags:?}: batch length"
        );
    }
    let in_stream_order = |printed: String| {
        let mut lines: Vec<String> = printed.lines().map(str::to_owned).collect();
        if rows == Rows::AnyOrder {
            lines.sort();
        }
        lines
    };
    let reference_rows = in_stream_order(replay(&root, up, &reference.batch, flags, "up-up"));
    let readers: &[(&str, &Path)] = if client_output {
        &[("up", up), ("oc", oc.as_path())]
    } else {
        &[("up", up)]
    };
    for &(label, reader) in readers {
        assert_eq!(
            in_stream_order(replay(
                &root,
                reader,
                &ours.batch,
                flags,
                &format!("oc-{label}")
            )),
            reference_rows,
            "{flags:?}: rows of the oc-written batch replayed by {label}"
        );
    }
}

#[test]
fn a_pull_sends_a_record_for_every_itemized_entry() {
    compare_with_upstream(&["-ai"]);
}

#[test]
fn a_pull_without_incremental_recursion_sends_a_record_for_every_itemized_entry() {
    compare_with_upstream(&["-ai", "--no-inc-recursive"]);
}

/// `-ii` adds a record for an entry that is up to date (generator.c:625,
/// `stdout_format_has_i > 1`).
#[test]
fn a_pull_with_double_itemize_sends_a_record_for_unchanged_entries_too() {
    compare_with_upstream(&["-aii"]);
    compare_with_upstream(&["-aii", "--no-inc-recursive"]);
}

/// Under `-c` a file with the same content and another time is not
/// transferred and still has a record of its own.
#[test]
fn a_checksum_pull_sends_a_record_for_a_time_only_change() {
    compare_with_upstream(&["-aic"]);
    compare_with_upstream(&["-aic", "--no-inc-recursive"]);
}

/// A hard-link follower that is linked, not transferred, has a record
/// (hlink.c:maybe_hard_link() calls itemize()). The receiver sends follower
/// records after the rest of their list, on a push as well, where 3.5.2 sends
/// each at its own place, so neither the order nor the batch length is
/// compared here.
#[test]
fn a_hard_link_pull_sends_a_record_for_a_linked_follower() {
    compare_rows(&["-aiH"], false, Rows::AnyOrder);
    compare_rows(&["-aiH", "--no-inc-recursive"], false, Rows::AnyOrder);
}

/// The records do not depend on the client asking for `-i`
/// (generator.c:2809-2810 sets `itemizing` at protocol 29 and above).
#[test]
fn a_pull_without_itemized_output_sends_the_same_records() {
    compare_with_upstream(&["-a"]);
    compare_records_with_upstream(&["-av"]);
}

#[test]
fn a_pull_through_the_other_receiver_drivers_sends_the_same_records() {
    compare_with_upstream(&["-ai", "--delete-after"]);
    compare_with_upstream(&["-ai", "--delay-updates"]);
    compare_with_upstream(&["-ai", "--protocol=30"]);
    compare_with_upstream(&["-ai", "--protocol=29"]);
}

/// A dry run prints the same rows as before: the server logs nothing for it,
/// so the client's output is the only thing to compare.
#[test]
fn a_dry_run_pull_prints_the_same_rows() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_source(&root);
    for flags in [&["-ain"][..], &["-ain", "--no-inc-recursive"]] {
        let tag = flags.join("");
        let reference = pull(&root, up, up, flags, &format!("up{tag}"));
        assert!(
            reference.stdout.contains("cd+++++++++ newdir/"),
            "3.5.2 {flags:?} must itemize: {}",
            reference.stdout
        );
        let ours = pull(&root, &oc, up, flags, &format!("oc{tag}"));
        assert_eq!(ours.stdout, reference.stdout, "{flags:?}: client output");
    }
}

#[test]
fn a_pull_from_rsync_3_5_1_sends_the_same_records() {
    for flags in [&["-ai"][..], &["-ai", "--no-inc-recursive"]] {
        compare_from(Some(UpstreamVersion::V3_5_1), flags, true, Rows::InOrder);
    }
}

#[test]
fn a_pull_from_this_server_sends_the_same_records() {
    for flags in [&["-ai"][..], &["-ai", "--no-inc-recursive"]] {
        compare_from(None, flags, true, Rows::InOrder);
    }
}

const WIDE_FILES: usize = 300;
const WIDE_DIRS: usize = 10;

fn wide_dir(index: usize) -> String {
    format!("d{:02}", index % WIDE_DIRS)
}

fn wide_name(index: usize) -> String {
    format!("{}/f{index:03}", wide_dir(index))
}

/// `src/` holds ten directories of thirty files each and a symlink in each.
fn build_wide_source(root: &Path) {
    let src = root.join("src");
    for dir in 0..WIDE_DIRS {
        fs::create_dir_all(src.join(wide_dir(dir))).expect("create dir");
    }
    for index in 0..WIDE_FILES {
        let path = src.join(wide_name(index));
        fs::write(&path, "same\n").expect("write file");
        set_mtime(&path, SOURCE_MTIME);
    }
    for dir in 0..WIDE_DIRS {
        let link = src.join(wide_dir(dir)).join("link");
        symlink(format!("f{dir:03}"), &link).expect("symlink");
        set_mtime(&link, SOURCE_MTIME);
        set_mtime(&src.join(wide_dir(dir)), SOURCE_MTIME);
    }
    set_mtime(&src, SOURCE_MTIME);
    write_remote_shell(root);
}

/// A destination for the wide source: of every three files one is up to
/// date, one differs in mode only and one in content; every directory has an
/// older time and no symlink. Under `-ii` two records in three ask for no
/// transfer, and they sit between the requests.
fn build_wide_destination(dst: &Path) {
    for dir in 0..WIDE_DIRS {
        fs::create_dir_all(dst.join(wide_dir(dir))).expect("create dir");
    }
    for index in 0..WIDE_FILES {
        let path = dst.join(wide_name(index));
        match index % 3 {
            0 => {
                fs::write(&path, "same\n").expect("write file");
                set_mtime(&path, SOURCE_MTIME);
            }
            1 => {
                fs::write(&path, "same\n").expect("write file");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
                set_mtime(&path, SOURCE_MTIME);
            }
            _ => {
                fs::write(&path, "older and longer\n").expect("write file");
                set_mtime(&path, DEST_MTIME);
            }
        }
    }
    for dir in 0..WIDE_DIRS {
        set_mtime(&dst.join(wide_dir(dir)), DEST_MTIME);
    }
    set_mtime(dst, DEST_MTIME);
}

/// More requests than the receiver keeps in flight at once, with twice as
/// many records between them that the sender only echoes: the echoes are
/// read in turn with the transfer replies and the pull completes, against
/// rsync 3.5.2 and against this binary as the server.
#[test]
fn echoes_between_more_requests_than_the_window_are_read_in_turn() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_wide_source(&root);

    for (served_by, server) in [("up", up), ("oc", oc.as_path())] {
        for flags in [&["-aii"][..], &["-aii", "--no-inc-recursive"]] {
            let tag = format!("{served_by}{}", flags.join(""));
            let reference = pull_into(
                &root,
                up,
                server,
                flags,
                &format!("up-{tag}"),
                build_wide_destination,
            );
            let expected = reference.records();
            let echoed = expected
                .iter()
                .filter(|(_, iflags, _)| iflags & ITEM_TRANSFER == 0)
                .count();
            // The root, ten directories, ten symlinks and two files in three.
            assert_eq!(
                (echoed, expected.len() - echoed),
                (1 + 2 * WIDE_DIRS + 2 * WIDE_FILES / 3, WIDE_FILES / 3),
                "3.5.2 {flags:?} from {served_by}: echoed records and requests"
            );
            let ours = pull_into(
                &root,
                &oc,
                server,
                flags,
                &format!("oc-{tag}"),
                build_wide_destination,
            );
            assert_eq!(
                ours.records(),
                expected,
                "{flags:?} from {served_by}: records"
            );
            assert_eq!(
                ours.server_rows, reference.server_rows,
                "{flags:?} from {served_by}: what the server logged"
            );
            assert_eq!(
                ours.stdout, reference.stdout,
                "{flags:?} from {served_by}: client output"
            );
        }
    }
}

/// `--delay-symlinks` creates symlinks after the transfers. The record of a
/// deferred symlink is still sent once, inside the transfer: rsync 3.5.2 has
/// no such option, so the reference is its plain run, and the records are
/// compared as a set because the deferred one goes out later. The option
/// turns incremental recursion off, so the reference runs without it.
#[test]
fn a_deferred_symlink_has_one_record_inside_the_transfer() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_source(&root);

    for inc in [&[][..], &["--no-inc-recursive"]] {
        let plain = ["-ai", "--no-inc-recursive"];
        let deferred = [&["-ai", "--delay-symlinks"][..], inc].concat();
        let tag = inc.join("");
        let mut reference = pull(&root, up, up, &plain, &format!("up{tag}"));
        // A record written after the transfer's closing NDX_DONE would make
        // the 3.5.2 server fail the run, which `pull` asserts it does not.
        let mut ours = pull(&root, &oc, up, &deferred, &format!("oc{tag}"));
        let symlink_rows = |rows: &[String]| {
            rows.iter()
                .filter(|row| row.as_str() == "cL+++++++++\\ l -> a")
                .count()
        };
        assert_eq!(symlink_rows(&reference.server_rows), 1, "3.5.2 {plain:?}");
        assert_eq!(symlink_rows(&ours.server_rows), 1, "{deferred:?}");
        let (mut sent, mut expected) = (ours.records(), reference.records());
        sent.sort();
        expected.sort();
        assert_eq!(sent, expected, "{deferred:?}: the records the client sent");
        reference.server_rows.sort();
        ours.server_rows.sort();
        assert_eq!(
            ours.server_rows, reference.server_rows,
            "{deferred:?}: what the server logged"
        );
    }
}
