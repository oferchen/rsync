//! Symlink and special-file surface: itemize emission for files,
//! directories, and symlinks, plus the `MsgInfoSender` plumbing that
//! receivers use to surface those events.

use std::io::{self, Write};

use protocol::flist::FileEntry;

use super::support::{test_config, test_handshake};
use crate::config::ServerConfig;
use crate::generator::ItemFlags;
use crate::writer::MsgInfoSender;

use super::super::ReceiverContext;

/// A test writer that records MSG_INFO payloads for verification.
struct MockMsgInfoWriter {
    messages: Vec<Vec<u8>>,
}

impl MockMsgInfoWriter {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }
}

impl Write for MockMsgInfoWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for MockMsgInfoWriter {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.messages.push(data.to_vec());
        Ok(())
    }
}

fn receiver_config_with_itemize() -> ServerConfig {
    let mut config = test_config();
    config.flags.info_flags.itemize = true;
    // Server mode (not client mode) to enable emission
    config.connection.client_mode = false;
    config
}

#[test]
fn render_itemize_new_file_transfer() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_file("docs/readme.txt".into(), 1024, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    // upstream: log.c:707-710 - a server-mode receiver is the remote end of a
    // push (the client is the sender), so the transfer glyph is `<`.
    assert_eq!(
        ctx.render_itemize_line(&iflags, &entry, None).as_deref(),
        Some("<f+++++++++ docs/readme.txt\n")
    );
}

#[test]
fn render_itemize_updated_file_transfer() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_file("data.bin".into(), 512, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER);

    // upstream: log.c:707-710 - server-mode receiver renders the push `<` glyph.
    assert_eq!(
        ctx.render_itemize_line(&iflags, &entry, None).as_deref(),
        Some("<f......... data.bin\n")
    );
}

#[test]
fn render_itemize_directory_creation() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_directory("subdir".into(), 0o755);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    assert_eq!(
        ctx.render_itemize_line(&iflags, &entry, None).as_deref(),
        Some("cd+++++++++ subdir/\n")
    );
}

#[test]
fn render_itemize_root_directory_emits_creation_glyph_when_freshly_created() {
    // Regression: upstream `testsuite/itemize.test` expects `cd+++++++++ ./`
    // as the second line of `-iplr from/ to/` against a non-existent dest.
    // The root entry (path == ".") arrives with iflags == 0 because oc-rsync's
    // create_directory_incremental cannot observe the pre-flight mkdir
    // performed by `ensure_dest_root_exists`. When that pre-flight actually
    // created the root (`dest_root_created == true`), mirror upstream
    // main.c:803-805 FLAG_DIR_CREATED by forcing the created-directory glyph.
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.dest_root_created = true;

    let entry = FileEntry::new_directory(".".into(), 0o755);
    let iflags = ItemFlags::from_raw(0);

    assert_eq!(
        ctx.render_itemize_line(&iflags, &entry, None).as_deref(),
        Some("cd+++++++++ ./\n")
    );
}

#[test]
fn render_itemize_root_directory_no_glyph_when_dest_root_preexisted() {
    // upstream main.c:803-805 only sets FLAG_DIR_CREATED when the receiver
    // had to mkdir the destination root. When the root already existed
    // (e.g. `up1/ -> up2/` where up2 is present), the flag stays clear and
    // the root reports a metadata-only row that the significance gate drops
    // (no `cd+++++++++ ./`). Without this, the exclude-lsh `--update` leg
    // wrongly itemized the existing root as freshly created.
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    // dest_root_created defaults to false (root pre-existed).

    let entry = FileEntry::new_directory(".".into(), 0o755);
    let iflags = ItemFlags::from_raw(0);

    assert_eq!(ctx.render_itemize_line(&iflags, &entry, None), None);
}

#[test]
fn render_itemize_up_to_date_file() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_file("unchanged.txt".into(), 256, 0o644);
    // No flags - file is up-to-date, no changes
    let iflags = ItemFlags::from_raw(0);

    // upstream: generator.c:574-576 - no line when iflags has no significant
    // flags (file is completely unchanged)
    assert_eq!(ctx.render_itemize_line(&iflags, &entry, None), None);
}

#[test]
fn emit_itemize_server_mode_does_not_forward_msg_info() {
    // upstream: log.c:822 gates the FCLIENT itemize write on `!am_server`, and
    // generator.c:583-599 writes the iflags over the wire so the client's
    // SENDER prints the push row (sender.c:461). A server-mode receiver (the
    // remote end of a push) must therefore forward NO pre-rendered MSG_INFO
    // row, or every pushed file would itemize twice against the sender's own
    // row.
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("docs/readme.txt".into(), 1024, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry, None)
        .unwrap();

    assert!(writer.messages.is_empty());
}

#[test]
fn emit_itemize_client_mode_uses_stdout_not_msg_info() {
    // A client receiver (pull) itemizes to its own stdout via emit_info_line,
    // never as a MSG_INFO frame - mirroring upstream log.c:rwrite() which
    // writes to the client fd when !am_server. So the MsgInfoSender writer
    // receives nothing even though the row was produced (to stdout).
    let handshake = test_handshake();
    let mut config = receiver_config_with_itemize();
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry, None)
        .unwrap();

    assert!(writer.messages.is_empty());
}

#[test]
fn out_format_collects_events_and_suppresses_string_path() {
    // A pulling client with a custom `--out-format` (out_format_active, no `-i`)
    // buffers a metadata-bearing event per logged entry instead of writing its
    // own itemize string, so the CLI can render the user's template. The default
    // string buffer stays empty and the drained event carries the raw fields
    // plus the client-direction (`>`) `%i` glyph.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.flags.info_flags.out_format_active = true;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_file("docs/readme.txt".into(), 1024, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.record_itemize(3, &iflags, &entry, None);

    // String path suppressed; event path populated.
    assert!(ctx.itemize_rows.borrow().is_empty());
    let rows = ctx.drain_event_rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, std::path::Path::new("docs/readme.txt"));
    assert_eq!(row.size, 1024);
    // Full mode (type + perms) is carried so the CLI can render `%B`/`%M`.
    assert_eq!(row.mode, 0o100_644);
    assert_eq!(row.mode & 0o777, 0o644);
    assert!(row.is_new);
    assert!(!row.is_dir);
    // upstream log.c:707-710 - a client-mode (pull) receiver renders the `>`
    // receive-direction glyph; the string is the fixed 11-char `%i`.
    assert_eq!(row.itemize.len(), 11);
    assert!(row.itemize.starts_with(">f"), "itemize = {:?}", row.itemize);
    // Drain is idempotent (buffer taken).
    assert!(ctx.drain_event_rows().is_empty());
}

#[test]
fn out_format_inactive_uses_string_path() {
    // Without out_format_active the pull receiver keeps its default behaviour:
    // `-i` rows buffer as strings and no events are collected.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = true;
    config.flags.info_flags.out_format_active = false;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.record_itemize(0, &iflags, &entry, None);

    assert!(ctx.drain_event_rows().is_empty());
    assert_eq!(ctx.itemize_rows.borrow().len(), 1);
}

#[test]
fn out_format_collects_symlink() {
    // A symlink is itemized through `emit_or_record_itemize` with its flist index.
    // Under a custom `--out-format` the index is carried into the collect buffer
    // instead of suppressing the row, so a pulling client renders the template for
    // the symlink in flist order alongside regular files - matching upstream's
    // single generate_files walk (generator.c:2329).
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.flags.info_flags.out_format_active = true;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_symlink("mylink".into(), "target".into());
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_or_record_itemize(&mut writer, 5, &iflags, &entry)
        .unwrap();

    // No immediate MSG_INFO string is written; the event is buffered instead.
    assert!(writer.messages.is_empty());
    assert!(ctx.itemize_rows.borrow().is_empty());
    let rows = ctx.drain_event_rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, std::path::Path::new("mylink"));
    assert!(row.is_symlink);
    assert_eq!(
        row.symlink_target.as_deref(),
        Some(std::path::Path::new("target"))
    );
    assert!(row.is_new);
    // upstream log.c - a newly created symlink shows the `c` local-change glyph
    // and the `L` symlink type; the row is the fixed 11-char `%i`.
    assert_eq!(row.itemize.len(), 11);
    assert!(row.itemize.starts_with("cL"), "itemize = {:?}", row.itemize);
}

#[test]
fn out_format_collects_special() {
    // A FIFO (special) is created on the same immediate path as a symlink and also
    // flows through `emit_or_record_itemize` into the collect buffer under a custom
    // `--out-format` rather than being dropped.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.flags.info_flags.out_format_active = true;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_fifo("apipe".into(), 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_or_record_itemize(&mut writer, 2, &iflags, &entry)
        .unwrap();

    assert!(writer.messages.is_empty());
    let rows = ctx.drain_event_rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, std::path::Path::new("apipe"));
    assert!(row.is_new);
    // upstream log.c - a newly created special shows the `c` local-change glyph
    // and the `S` special type.
    assert_eq!(row.itemize.len(), 11);
    assert!(row.itemize.starts_with("cS"), "itemize = {:?}", row.itemize);
}

#[test]
fn out_format_pull_hardlink_follower_carries_leader() {
    // upstream: hlink.c:232-234 - create_hardlinks itemizes each follower with the
    // leader's transfer-relative name (`xname`), which log.c:643-646 renders as the
    // `%L` ` => <leader>` suffix. On a pull under a custom `--out-format` the row is
    // collected as an event rather than written as a string; the xname must ride
    // into `hardlink_leader` so `%L` is not empty (#256). This asserts the receiver
    // populates that field from the same xname `emit_itemize_indexed` receives.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.flags.info_flags.out_format_active = true;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("dir/alias.txt".into(), 4, 0o644);
    let iflags = ItemFlags::from_raw(
        ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_XNAME_FOLLOWS | ItemFlags::ITEM_IS_NEW,
    );

    ctx.emit_itemize_indexed(&mut writer, 2, &iflags, &entry, Some(b"dir/leader.txt"))
        .unwrap();

    // String path suppressed; the collected event carries the leader for `%L`.
    assert!(ctx.itemize_rows.borrow().is_empty());
    let rows = ctx.drain_event_rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.name, std::path::Path::new("dir/alias.txt"));
    assert_eq!(
        row.hardlink_leader.as_deref(),
        Some(std::path::Path::new("dir/leader.txt"))
    );
    assert!(row.itemize.starts_with("hf"), "itemize = {:?}", row.itemize);
}

#[test]
fn out_format_pull_empty_xname_carries_no_leader() {
    // upstream: log.c:676 gates the ` => hlink` suffix on `hlink && *hlink`, so an
    // already-correct hard-link alias itemized with the empty xname (hlink.c:218-222)
    // carries no leader and `%L` stays empty. An empty xname must not populate
    // `hardlink_leader`.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.flags.info_flags.out_format_active = true;
    config.connection.client_mode = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("dir/alias.txt".into(), 4, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize_indexed(&mut writer, 2, &iflags, &entry, Some(b""))
        .unwrap();

    let rows = ctx.drain_event_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].hardlink_leader, None);
}

#[test]
fn indexed_emit_off_out_format_uses_immediate_path() {
    // `emit_itemize_indexed` is the hardlink-follower path. Off a custom
    // `--out-format` it must behave exactly like the immediate `emit_itemize`:
    // with `-i` on a server receiver it renders no client row (the row travels as
    // wire iflags) and collects nothing.
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("leader.txt".into(), 10, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize_indexed(&mut writer, 1, &iflags, &entry, None)
        .unwrap();

    // Server-mode receiver writes no client-visible row and collects nothing.
    assert!(writer.messages.is_empty());
    assert!(ctx.drain_event_rows().is_empty());
    assert!(ctx.itemize_rows.borrow().is_empty());
}

#[test]
fn default_i_symlink_defers_into_flist_order_on_pull() {
    // On a client-mode pull the receiver defers itemize rows (defer_itemize) and
    // flushes them in flist-index order. A symlink created in the create_symlinks
    // pass must join that deferred buffer keyed by its flist index - not emit
    // immediately - so it interleaves with regular files exactly as upstream's
    // single generate_files walk does (generator.c:2329). Regression for the bug
    // where symlinks/specials printed before all regular files under `-i`.
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = true;
    config.connection.client_mode = true;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.defer_itemize = true;
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_symlink("blink".into(), "afile.txt".into());
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_or_record_itemize(&mut writer, 7, &iflags, &entry)
        .unwrap();

    // Nothing written immediately; the row is buffered for the ordered flush.
    assert!(writer.messages.is_empty());
    let buffered = ctx.itemize_rows.borrow();
    let rows = buffered.get(&7).expect("row buffered at flist index 7");
    assert_eq!(rows.len(), 1);
    // The deferred string carries the `cL` glyph and the ` -> target` suffix.
    assert!(rows[0].starts_with("cL"), "row = {:?}", rows[0]);
    assert!(
        rows[0].contains("blink -> afile.txt"),
        "row = {:?}",
        rows[0]
    );
}

#[test]
fn emit_itemize_skipped_without_itemize_flag() {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.connection.client_mode = false;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry, None)
        .unwrap();

    assert!(writer.messages.is_empty());
}

#[test]
fn render_itemize_symlink_with_target() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let entry = FileEntry::new_symlink("mylink".into(), "target".into());
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    assert_eq!(
        ctx.render_itemize_line(&iflags, &entry, None).as_deref(),
        Some("cL+++++++++ mylink -> target\n")
    );
}

#[test]
fn should_emit_itemize_conditions() {
    let handshake = test_handshake();

    // Server mode + itemize -> true
    let mut config = test_config();
    config.connection.client_mode = false;
    config.flags.info_flags.itemize = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(ctx.should_emit_itemize());

    // Client mode + itemize -> true. A client receiver (pull, where oc is the
    // generator) itemizes to its own stdout via emit_info_line, mirroring
    // upstream log.c:rwrite() which writes to the client fd when !am_server.
    // Emission is gated only on the itemize flag, not the role.
    let mut config = test_config();
    config.connection.client_mode = true;
    config.flags.info_flags.itemize = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(ctx.should_emit_itemize());

    // Server mode + no itemize -> false
    let mut config = test_config();
    config.connection.client_mode = false;
    config.flags.info_flags.itemize = false;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(!ctx.should_emit_itemize());
}

#[test]
fn msg_info_sender_default_noop() {
    // Verify that a bare Write impl with no MsgInfoSender override
    // uses the default no-op behavior
    struct PlainWriter;
    impl Write for PlainWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl MsgInfoSender for PlainWriter {}

    let mut w = PlainWriter;
    // Default impl should succeed silently
    w.send_msg_info(b"test data").unwrap();
}

/// With `--backup`, replacing an existing destination symlink must preserve the
/// old link (its target intact) at the backup path before the new one is
/// created. upstream: generator.c:2018-2020 - `atomic_create` calls
/// `make_backup` before removing the obstacle; without this the receiver would
/// unlink the old symlink and lose the user's prior target silently.
#[test]
#[cfg(unix)]
fn receiver_backs_up_existing_symlink_before_replacing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // An existing symlink with the OLD target occupies the destination path.
    std::os::unix::fs::symlink("old-target", dest.join("mylink")).expect("seed old symlink");

    let mut config = test_config();
    config.flags.links = true;
    config.flags.backup = true;
    config.connection.client_mode = false;

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_symlink("mylink".into(), "new-target".into())];

    let mut writer = MockMsgInfoWriter::new();
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed");

    // The new target replaced the old link.
    assert_eq!(
        std::fs::read_link(dest.join("mylink")).expect("new symlink must exist"),
        std::path::Path::new("new-target"),
        "the receiver must install the new symlink target",
    );
    // The old link survives at the ~ backup with its original target.
    assert_eq!(
        std::fs::read_link(dest.join("mylink~")).expect("backup symlink must exist"),
        std::path::Path::new("old-target"),
        "the prior symlink target must be preserved in the ~ backup",
    );
}

/// A pull-mode receiver context with `-i`, `-l`, and `-t` that defers itemize
/// rows so tests can read them back in flist-index order.
#[cfg(unix)]
fn repointed_symlink_ctx(file_list: Vec<FileEntry>) -> ReceiverContext {
    let mut config = test_config();
    config.flags.links = true;
    config.flags.times = true;
    config.flags.info_flags.itemize = true;
    config.connection.client_mode = true;
    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.defer_itemize = true;
    ctx.file_list = file_list;
    ctx
}

/// #241/#248: replacing a destination symlink that points elsewhere must
/// itemize as a CHANGE against the pre-replace lstat - `cLc........` - and
/// must NOT bump the created-symlinks tally.
///
/// upstream: generator.c:1604-1610 - after `atomic_create`, `itemize()` runs
/// with `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE` and `statret == 0` (the old
/// destination entry IS a symlink), so ITEM_IS_NEW never fires and the row
/// carries attribute dots, not `+++++++++`. receiver.c:733-741 counts creates
/// only from ITEM_IS_NEW.
#[test]
#[cfg(unix)]
fn replaced_symlink_itemizes_as_change_not_create() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    std::os::unix::fs::symlink("old-target", dest.join("mylink")).expect("seed old symlink");
    let old_meta = std::fs::symlink_metadata(dest.join("mylink")).expect("lstat old symlink");

    let mut entry = FileEntry::new_symlink("mylink".into(), "new-target".into());
    // Same whole second as the on-disk link: `t` must stay dark (same_time()
    // compares whole seconds at modify_window 0, util1.c:1478).
    entry.set_mtime(old_meta.mtime(), 0);
    let ctx = repointed_symlink_ctx(vec![entry]);

    let mut writer = MockMsgInfoWriter::new();
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed");

    let buffered = ctx.itemize_rows.borrow();
    let rows = buffered.get(&0).expect("row buffered at flist index 0");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].starts_with("cLc........ "),
        "a re-pointed symlink is a change row, not all-new: {:?}",
        rows[0]
    );
    assert!(
        rows[0].contains("mylink -> new-target"),
        "row = {:?}",
        rows[0]
    );
    assert_eq!(
        ctx.created_stats.get().symlinks,
        0,
        "a replaced symlink is not a creation (receiver.c:733-741)"
    );
}

/// Control for #241/#248: a genuinely absent destination symlink still renders
/// the all-new row (`cL+++++++++`) and bumps the created tally.
///
/// upstream: generator.c:1608 with `statret < 0` -> itemize() adds ITEM_IS_NEW
/// (generator.c:578); receiver.c:733-741 counts it.
#[test]
#[cfg(unix)]
fn absent_symlink_still_itemizes_as_all_new() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let entry = FileEntry::new_symlink("mylink".into(), "new-target".into());
    let ctx = repointed_symlink_ctx(vec![entry]);

    let mut writer = MockMsgInfoWriter::new();
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed");

    let buffered = ctx.itemize_rows.borrow();
    let rows = buffered.get(&0).expect("row buffered at flist index 0");
    assert!(
        rows[0].starts_with("cL+++++++++ "),
        "an absent symlink is the true all-new case: {:?}",
        rows[0]
    );
    assert_eq!(ctx.created_stats.get().symlinks, 1, "created_symlinks");
}

/// A non-symlink obstacle replaced by a symlink renders all-new AND counts as
/// a creation: upstream forces `statret = -1` when `stype != FT_SYMLINK`
/// (generator.c:1606-1607), so the wire carries ITEM_IS_NEW and
/// receiver.c:733-741 bumps created_symlinks.
#[test]
#[cfg(unix)]
fn non_symlink_obstacle_replacement_itemizes_as_all_new() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();
    std::fs::write(dest.join("mylink"), b"obstacle").expect("seed obstacle file");

    let entry = FileEntry::new_symlink("mylink".into(), "new-target".into());
    let ctx = repointed_symlink_ctx(vec![entry]);

    let mut writer = MockMsgInfoWriter::new();
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed");

    assert_eq!(
        std::fs::read_link(dest.join("mylink")).expect("symlink replaces obstacle"),
        std::path::Path::new("new-target"),
    );
    let buffered = ctx.itemize_rows.borrow();
    let rows = buffered.get(&0).expect("row buffered at flist index 0");
    assert!(
        rows[0].starts_with("cL+++++++++ "),
        "a replaced non-symlink obstacle renders all-new (statret forced to -1): {:?}",
        rows[0]
    );
    assert_eq!(
        ctx.created_stats.get().symlinks,
        1,
        "an obstacle replacement is ITEM_IS_NEW and counts as created"
    );
}
