use super::common::*;
use super::*;

#[test]
fn transfer_request_with_filter_excludes_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        OsString::from("--filter"),
        OsString::from("!"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_merge_applies_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        filter_arg,
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_merge_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"log").expect("write log");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n!\n- *.log\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        filter_arg,
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_request_with_filter_protect_preserves_destination_entry() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let dest_subdir = dest_root.join("source");
    std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
    std::fs::write(dest_subdir.join("keep.txt"), b"keep").expect("write dest keep");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("--filter"),
        OsString::from("protect keep.txt"),
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
}

/// upstream: exclude.c:1619-1627 `parse_filter_file()` bounds the merge nesting
/// DEPTH at `MAX_MERGE_DEPTH` (exclude.c:168) rather than detecting a cycle, and
/// exits `RERR_FILEIO` under `XFLG_FATAL_ERRORS`.
///
/// WHY the depth cap and not a cycle test: a self-including file is only the
/// easiest way to exceed the bound. A chain of 40 distinct files that never
/// repeats is equally unbounded, and no "have I seen this file?" test refuses
/// it. Pinning the wording as well as the exit code keeps oc's diagnostic
/// comparable with rsync's for an operator debugging a filter set.
#[test]
fn transfer_request_with_filter_merge_bounds_include_depth() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, format!("merge {}\n", filter_file.display()))
        .expect("write recursive filter");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ]);

    assert_eq!(
        code, 11,
        "upstream: exclude.c:1626 exit_cleanup(RERR_FILEIO)"
    );
    assert!(stdout.is_empty());
    let rendered = String::from_utf8_lossy(&stderr);
    assert!(
        rendered.contains("merge-file include depth limit (32) exceeded"),
        "expected upstream's depth-limit diagnostic, got: {rendered}"
    );
}

/// The class-level companion: no file is merged twice, so a cycle test cannot
/// refuse this chain at any length. Only the depth bound can. This is the case
/// that distinguishes upstream's rule from the one oc used to implement, and
/// without it the fix could regress to a cycle set and still look correct.
#[test]
fn transfer_request_with_filter_merge_bounds_an_acyclic_chain() {
    use tempfile::tempdir;

    const CHAIN: usize = filters::MAX_MERGE_DEPTH + 4;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    // link[i] merges link[i + 1]; the last link is an inert comment. Every file
    // is distinct, so `visited` never sees a repeat.
    let link = |i: usize| tmp.path().join(format!("link{i}.txt"));
    for i in 0..CHAIN {
        std::fs::write(link(i), format!("merge {}\n", link(i + 1).display()))
            .expect("write chain link");
    }
    std::fs::write(link(CHAIN), "# end of chain\n").expect("write chain tail");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from(format!("merge {}", link(0).display())),
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ]);

    assert_eq!(code, 11, "an acyclic chain must still hit the depth cap");
    let rendered = String::from_utf8_lossy(&stderr);
    assert!(
        rendered.contains("merge-file include depth limit (32) exceeded"),
        "expected upstream's depth-limit diagnostic, got: {rendered}"
    );
}

/// Non-vacuity companion: a chain shorter than the cap must parse cleanly, so
/// the two tests above cannot pass merely because merge chains are broken.
#[test]
fn transfer_request_with_filter_merge_allows_a_chain_within_the_cap() {
    use tempfile::tempdir;

    const CHAIN: usize = filters::MAX_MERGE_DEPTH - 2;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let link = |i: usize| tmp.path().join(format!("link{i}.txt"));
    for i in 0..CHAIN {
        std::fs::write(link(i), format!("merge {}\n", link(i + 1).display()))
            .expect("write chain link");
    }
    std::fs::write(link(CHAIN), "- *.tmp\n").expect("write chain tail");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from(format!("merge {}", link(0).display())),
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ]);

    let rendered = String::from_utf8_lossy(&stderr);
    assert_eq!(code, 0, "chain within the cap must parse: {rendered}");
    assert!(
        !rendered.contains("merge-file include depth limit"),
        "unexpected depth diagnostic: {rendered}"
    );
}

/// upstream: exclude.c:1393-1402 resolves `FILTRULE_CLEAR_LIST` via
/// `pop_filter_list` which truncates only the local section of the per-merge-file
/// rule list. A `!` inside a merge file must not clear parent CLI `--filter`
/// rules collected before the merge directive.
#[test]
fn transfer_request_with_filter_merge_bang_preserves_parent_cli_rule() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip tmp").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"skip log").expect("write log");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "!\n- *.log\n").expect("write merge file");

    let merge_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        OsString::from("--filter"),
        merge_arg,
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    // Parent `--filter "- *.tmp"` survived the merge file's `!`.
    assert!(!copied_root.join("skip.tmp").exists());
    // Merge file's own exclude still applies.
    assert!(!copied_root.join("skip.log").exists());
}

/// upstream: exclude.c:1393-1402 - a `!` inside a nested merge file clears only
/// the nested scope. Rules added by the outer merge file before the nested
/// reference survive.
#[test]
fn transfer_request_with_filter_nested_merge_bang_preserves_outer_scope() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip tmp").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"skip log").expect("write log");

    let inner = tmp.path().join("inner.txt");
    let outer = tmp.path().join("outer.txt");
    std::fs::write(&inner, "!\n- *.log\n").expect("write inner");
    std::fs::write(&outer, format!("- *.tmp\nmerge {}\n", inner.display())).expect("write outer");

    let merge_arg = OsString::from(format!("merge {}", outer.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        merge_arg,
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    // Outer merge's exclude survives the inner merge's `!`.
    assert!(!copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.log").exists());
}

/// upstream: exclude.c:1393-1402 - `!` inside an `--exclude-from FILE` clears
/// only the scope of that file. Parent `--filter`/`--exclude` CLI rules survive.
#[test]
fn transfer_request_with_exclude_from_bang_preserves_parent_cli_rule() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip tmp").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"skip log").expect("write log");

    let exclude_file = tmp.path().join("excludes.txt");
    std::fs::write(&exclude_file, "!\n*.log\n").expect("write exclude file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // filter rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        OsString::from("--exclude-from"),
        exclude_file.clone().into_os_string(),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    // Parent `--filter "- *.tmp"` survived the exclude-from file's `!`.
    assert!(!copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.log").exists());
}

/// upstream: exclude.c:1399 pop_filter_list - a bare `!` inside `$CVSIGNORE`
/// (or `~/.cvsignore`) keeps FILTRULE_CLEAR_LIST and clears the ENTIRE shared
/// cvs_filter_list, so the built-in CVS_EXCLUDE_PATTERNS collected before it
/// (e.g. `core`) are wiped too. Only patterns added after the `!` survive.
#[test]
fn transfer_request_with_cvsignore_bang_wipes_default_cvs_patterns() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    // A leading bare `!` clears the whole shared CVS list, then `*.skip` is the
    // only surviving exclude.
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new("! *.skip"));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    // `core` is a built-in CVS_EXCLUDE_PATTERN, but the bare `!` wipes it, so it
    // must now be transferred.
    std::fs::write(source_root.join("core"), b"core dump").expect("write core");
    std::fs::write(source_root.join("drop.skip"), b"drop").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        // recurse defaults to 0 (options.c:112); -r walks source/ so the
        // cvs-exclude rules are actually exercised, mirroring upstream.
        OsString::from("--recursive"),
        OsString::from("--cvs-exclude"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    // The built-in `core` pattern was wiped by the `$CVSIGNORE` bare `!`.
    assert!(copied_root.join("core").exists());
    // `$CVSIGNORE`'s own pattern (after the `!`) still excludes `*.skip`.
    assert!(!copied_root.join("drop.skip").exists());
}
