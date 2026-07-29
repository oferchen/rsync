//! Regression: the plain-`-v` directory NAME lines on a remote pull must be
//! gated exactly like upstream `generator.c:1503-1505` - a directory is named
//! only when `set_file_attrs()` changed it (newly created, or a pre-transfer
//! attribute differs). Previously the pipelined receiver named every directory
//! in the file list unconditionally, printing a spurious `./` for the unchanged
//! transfer root and re-printing every unchanged directory on a re-sync.

use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

fn ctx_with(entries: Vec<FileEntry>, times: bool) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flags: ParsedServerFlags {
            verbose: true,
            times,
            perms: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut c = ReceiverContext::new_for_test(&handshake, config);
    c.file_list = entries;
    c.config.connection.client_mode = true;
    c
}

fn dir(name: &str, mtime: i64) -> FileEntry {
    let mut e = FileEntry::new_directory(PathBuf::from(name), 0o755);
    e.set_mtime(mtime, 0);
    e
}

/// A directory absent from the destination (fresh transfer) is always named;
/// an existing directory whose mtime matches the source stays silent; one
/// whose mtime differs is named. The created transfer root prints `./`.
#[test]
fn names_only_created_or_changed_dirs() {
    let dest = tempfile::tempdir().unwrap();
    let t: i64 = 1_000_000_000;

    // "keep" exists with a matching mtime; "changed" exists with a differing
    // mtime; "new" is absent (will be created).
    std::fs::create_dir(dest.path().join("keep")).unwrap();
    filetime::set_file_mtime(
        dest.path().join("keep"),
        filetime::FileTime::from_unix_time(t, 0),
    )
    .unwrap();
    std::fs::create_dir(dest.path().join("changed")).unwrap();
    filetime::set_file_mtime(
        dest.path().join("changed"),
        filetime::FileTime::from_unix_time(t, 0),
    )
    .unwrap();

    let entries = vec![
        dir(".", t),             // idx 0 - root
        dir("keep", t),          // idx 1 - unchanged, must be skipped
        dir("changed", t + 500), // idx 2 - mtime differs, named
        dir("new", t),           // idx 3 - absent, named as created
    ];
    let mut c = ctx_with(entries, true);
    // The pre-flight mkdir created the root this run, so upstream names it.
    c.dest_root_created = true;

    let lines = c.verbose_dir_name_lines(dest.path());
    assert_eq!(
        lines,
        vec![
            (0usize, "./".to_string()),
            (2usize, "changed/".to_string()),
            (3usize, "new/".to_string()),
        ],
    );
}

/// A dry run against a missing destination root still names `./`, on both
/// receiver drivers (this helper is shared; the CI feature matrix runs it with
/// `incremental-flist` on and off). Upstream "creates" the missing root even
/// under `-n` (`main.c:796-808`; `do_mkdir` is a dry-run no-op), and
/// `FLAG_DIR_CREATED` then forces the NAME row (`generator.c:1465-1466`,
/// `generator.c:1503-1505`; `rsync.c:498-499` returns 1 for the missing dest
/// under dry-run). `--list-only` never reaches that mkdir (`main.c:743`), so
/// the root stays silent there.
#[test]
fn missing_dest_root_is_named_on_dry_run_but_not_list_only() {
    let t: i64 = 1_000_000_000;
    let entries = vec![dir(".", t), dir("sub", t)];
    let mut c = ctx_with(entries, true);
    c.config.flags.dry_run = true;

    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("nonexistent-dest");
    assert_eq!(
        c.verbose_dir_name_lines(&missing),
        vec![(0usize, "./".to_string()), (1usize, "sub/".to_string())],
        "a dry run against a missing dest root must name ./ first",
    );

    c.config.flags.dry_run = false;
    c.config.flags.list_only = true;
    assert!(
        !c.verbose_dir_name_lines(&missing)
            .iter()
            .any(|(_, name)| name == "./"),
        "--list-only never creates the dest root, so ./ must stay silent",
    );
}

/// With `--times` off, an existing directory reports no time change, so an
/// unchanged re-sync names nothing (upstream prints no directory lines when
/// set_file_attrs() makes no change). The absent root is not created here.
#[test]
fn unchanged_resync_names_nothing() {
    let dest = tempfile::tempdir().unwrap();
    std::fs::create_dir(dest.path().join("adir")).unwrap();
    std::fs::create_dir(dest.path().join("bdir")).unwrap();

    let entries = vec![dir("adir", 0), dir("bdir", 0)];
    let c = ctx_with(entries, false);

    assert!(
        c.verbose_dir_name_lines(dest.path()).is_empty(),
        "an unchanged re-sync must name no directories",
    );
}
