//! UTS regression test for `files-from.test` invocation 2:
//! `oc-rsync -avse lsh.sh --files-from=LIST source localhost:dest/`.
//!
//! Before the fix, the local SSH-push generator did not learn about the
//! local `--files-from` file. It would recurse the source operand (an
//! absolute path on disk) and emit wire entries whose relative names still
//! carried the absolute prefix. The remote receiver - which sees implied
//! `--relative` because `--files-from` is active - would then create
//! `./home/runner/work/rsync/rsync/target/interop/upstream-testsuite/...`
//! at the destination instead of the listed paths (`./dir`, `./empty`,
//! `./emptydir`, ...).
//!
//! This test rebuilds the same generator pipeline that
//! `core::client::remote::ssh_transfer::build_server_config_for_generator`
//! produces after the fix: `files_from_path = Some(<local list>)` plus the
//! source root as the only positional arg. It asserts that the resulting
//! wire-side file list contains the entries from the list, with names
//! relative to the destination - never the absolute source path.
//!
//! # Upstream Reference
//!
//! - `options.c:2473,2501` - filesfrom_fd opens stdin or the local file.
//! - `flist.c:2275-2298` - send_file_list() chdirs to argv[0] and reads
//!   one filename at a time from `filesfrom_fd`.
//! - `flist.c:2316-2330` - per-entry `/./` anchor splits the prefix into
//!   the per-entry walk base; the suffix is the wire-side relative name.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use protocol::ProtocolVersion;
use transfer::{
    GeneratorContext, HandshakeResult, ServerConfig, ServerRole, TransferPhase, TransferPipeline,
};

/// Builds a `ServerConfig::Generator` that mirrors the shape produced by
/// the SSH-push path after the fix in `build_server_config_for_generator`.
fn ssh_push_generator_config(source_root: &Path, list_path: &Path) -> ServerConfig {
    let flag_string = "-r.iLsfxCIvu".to_owned();
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        flag_string,
        vec![source_root.to_path_buf().into_os_string()],
    )
    .expect("server config");

    config.file_selection.files_from_path = Some(list_path.to_string_lossy().into_owned());
    // The upstream files-from test uses newline-delimited lists, not NUL.
    config.file_selection.from0 = false;
    config.connection.client_mode = true;
    config
}

/// Builds a minimal handshake matching protocol 32 (current upstream).
fn test_handshake() -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: true,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Creates a transfer pipeline already advanced to `FileListTransfer`, the
/// state expected by `build_file_list_with_base`.
fn test_pipeline() -> TransferPipeline {
    let mut pipeline = TransferPipeline::new(ServerRole::Generator);
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .expect("advance to FilterExchange");
    pipeline
        .advance_to(TransferPhase::FileListTransfer)
        .expect("advance to FileListTransfer");
    pipeline
}

/// Recreates the upstream `files-from.test` source layout (the subset the
/// local-mode pass already exercises): a `from/` subdirectory holding
/// `dir/subdir/{subsubdir,subsubdir2}/...`.
fn seed_files_from_test_source(root: &std::path::Path) {
    let from = root.join("from");
    let subdir = from.join("dir").join("subdir");
    let subsubdir = subdir.join("subsubdir");
    let subsubdir2 = subdir.join("subsubdir2");
    fs::create_dir_all(&subsubdir).expect("mkdir subsubdir");
    fs::create_dir_all(&subsubdir2).expect("mkdir subsubdir2");

    fs::write(subdir.join("foobar.baz"), b"foobar payload").expect("write foobar.baz");
    fs::write(subsubdir.join("inner.txt"), b"inner").expect("write inner.txt");
    fs::write(subsubdir2.join("trailing.txt"), b"trailing").expect("write trailing.txt");
}

/// The bug surface: the wire-side relative names of the generator's file
/// list must NOT include the absolute prefix of the source directory. The
/// destination receiver writes paths relative to its destination root, so
/// any wire entry whose name starts with the source's absolute path would
/// reproduce the original failure (`./home/runner/work/...`).
#[test]
fn ssh_push_files_from_emits_relative_wire_names() {
    let scratch = TempDir::new().expect("tempdir");
    seed_files_from_test_source(scratch.path());

    let list_path = scratch.path().join("filelist");
    // Same payload as the upstream `files-from.test` 4-invocation matrix.
    fs::write(
        &list_path,
        "from/./\n\
         from/./dir/subdir\n\
         from/./dir/subdir/subsubdir\n\
         from/./dir/subdir/subsubdir2/\n\
         from/./dir/subdir/foobar.baz\n",
    )
    .expect("write filelist");

    let source_root = scratch.path().to_path_buf();
    let config = ssh_push_generator_config(&source_root, &list_path);
    let handshake = test_handshake();
    let pipeline = test_pipeline();

    let mut ctx = GeneratorContext::new(&handshake, config, pipeline);
    let paths = vec![source_root.clone()];

    let mut reader = std::io::Cursor::new(Vec::<u8>::new());
    let entries = ctx
        .resolve_files_from_paths(&paths, &mut reader)
        .expect("resolve_files_from_paths must succeed");
    assert!(
        !entries.is_empty(),
        "the generator must read at least one entry from the local --files-from list"
    );

    // Drive the production file-list builder with the resolved entries.
    let base_dir = paths.first().expect("first path").clone();
    ctx.build_file_list_with_base(&base_dir, &entries)
        .expect("build_file_list_with_base");

    let file_list = ctx.file_list();
    assert!(
        !file_list.is_empty(),
        "the file list must contain entries; the generator produced none"
    );

    // The source root is an absolute filesystem path. Any wire-side name
    // that starts with its absolute prefix would mirror the source's full
    // path on the destination - the original SSH-push regression.
    let absolute_prefix = source_root.to_string_lossy().into_owned();
    let stripped_prefix = absolute_prefix.trim_start_matches('/');

    let mut saw_dir_subdir = false;
    let mut saw_foobar = false;

    for entry in file_list {
        let name = entry.name();
        assert!(
            !name.starts_with('/'),
            "wire-side name must be relative, got absolute path: {name:?}"
        );
        assert!(
            !name.contains(stripped_prefix),
            "wire-side name must not embed the absolute source prefix \
             {stripped_prefix:?}, got: {name:?}"
        );
        assert!(
            !name.contains(&*absolute_prefix),
            "wire-side name must not embed the absolute source path, got: {name:?}"
        );

        if name == "dir/subdir" || name == "./dir/subdir" {
            saw_dir_subdir = true;
        }
        if name == "dir/subdir/foobar.baz" || name == "./dir/subdir/foobar.baz" {
            saw_foobar = true;
        }
    }

    assert!(
        saw_dir_subdir,
        "expected wire-side entry 'dir/subdir' (relative to destination root); \
         file list was: {:?}",
        file_list.iter().map(|e| e.name()).collect::<Vec<_>>()
    );
    assert!(
        saw_foobar,
        "expected wire-side entry 'dir/subdir/foobar.baz' (relative to \
         destination root); file list was: {:?}",
        file_list.iter().map(|e| e.name()).collect::<Vec<_>>()
    );
}

/// The pre-fix bug shape: when the generator config does NOT carry the
/// local `--files-from` path, the SSH-push sender falls back to walking
/// the source operand recursively. The wire-side names then come from a
/// recursive walk of the source root, not from the requested entries.
///
/// This case is the inverse of the regression test above: it pins what
/// the broken wiring would have produced so a future refactor that
/// re-introduces the bug (silently dropping `files_from_path` in
/// `build_server_config_for_generator`) flips the assertion in the
/// previous test.
#[test]
fn ssh_push_without_files_from_path_walks_source_recursively() {
    let scratch = TempDir::new().expect("tempdir");
    seed_files_from_test_source(scratch.path());

    let source_root = scratch.path().to_path_buf();
    // Deliberately do NOT populate `files_from_path` - this mirrors the
    // pre-fix wiring that produced the SSH-push failure.
    let flag_string = "-r.iLsfxCIvu".to_owned();
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        flag_string,
        vec![source_root.clone().into_os_string()],
    )
    .expect("server config");
    config.connection.client_mode = true;
    assert!(
        config.file_selection.files_from_path.is_none(),
        "pre-fix wiring leaves files_from_path empty"
    );

    let handshake = test_handshake();
    let pipeline = test_pipeline();
    let ctx = GeneratorContext::new(&handshake, config, pipeline);
    let paths = vec![source_root.clone()];

    let mut reader = std::io::Cursor::new(Vec::<u8>::new());
    let entries = ctx
        .resolve_files_from_paths(&paths, &mut reader)
        .expect("resolve_files_from_paths must succeed");
    assert!(
        entries.is_empty(),
        "with no files_from_path the resolver must return no entries; \
         the generator would fall back to build_file_list()"
    );
}
