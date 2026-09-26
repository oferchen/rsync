// `--files-from` entries under `--confine-root`.
//
// The files-from base is operator-selected but every list entry may not be, so
// upstream 3.5.1 resolves each entry with the ownership walk and refuses one
// whose resolved path leaves the confinement root
// (`rsync-3.5.1/syscall.c:149` `filesfrom_owner_walk_active()`,
// `rsync-3.5.1/flist.c:400-433` `filesfrom_link_stat()`). The escape shape is a
// TRUSTED-owned symlink inside the root pointing outside it: the ownership rule
// follows it by design, so only the confinement judgement can refuse it. A
// single-uid test owns every symlink it plants, so these cells pin that
// confinement half; the untrusted-owner half needs a second uid and stays with
// the root leg of the upstream `relative-source-ancestor` test.
//
// The cells reuse `install_backup_confinement` (backups.rs): the session root is
// process-global and every cell that installs one must share one lock.

/// A confinement root holding an in-root directory, plus trusted links to an
/// in-root directory, an out-of-root directory, and the root's own parent.
#[cfg(unix)]
struct FilesFromConfinementFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

#[cfg(unix)]
impl FilesFromConfinementFixture {
    fn new() -> Self {
        let temp = test_support::create_tempdir();
        let base = temp.path().to_path_buf();
        let root = base.join("confined-root");
        let outside = base.join("confined-outside");
        fs::create_dir_all(root.join("inside")).expect("create in-root dir");
        fs::create_dir_all(&outside).expect("create out-of-root dir");
        fs::write(root.join("inside/marker"), b"inside contents").expect("write inside");
        fs::write(outside.join("marker"), b"outside contents").expect("write outside");
        std::os::unix::fs::symlink(&outside, root.join("outside-link")).expect("outside link");
        std::os::unix::fs::symlink(root.join("inside"), root.join("inside-link"))
            .expect("inside link");
        std::os::unix::fs::symlink(&base, root.join("ancestor-link")).expect("ancestor link");
        Self {
            _temp: temp,
            root,
            outside,
        }
    }

    /// Runs `oc-rsync -r --files-from=<entry> <root>/ <root>/<dest>/` the way
    /// the client drive expands the list: each entry becomes `<base>/./<entry>`,
    /// or `<base>/<entry>` when the entry carries its own `/./` marker.
    fn transfer(&self, entry: &str, dest: &str, recursive: bool) -> (PathBuf, bool) {
        let dest = self.root.join(dest);
        fs::create_dir_all(&dest).expect("create dest");
        let mut operand = self.root.clone().into_os_string();
        operand.push(if entry.contains("/./") { "/" } else { "/./" });
        operand.push(entry);
        let mut dest_operand = dest.clone().into_os_string();
        dest_operand.push("/");
        let plan = LocalCopyPlan::from_operands_with_relative(&[operand, dest_operand], true)
            .expect("plan");
        let options = LocalCopyOptions::default()
            .relative_paths(true)
            .recursive(recursive)
            .dirs(!recursive)
            .files_from(true);
        let ok = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .is_ok();
        (dest, ok)
    }
}

/// WITNESS. A file entry reached through a trusted in-root link that points
/// outside `--confine-root` is refused and its content never copied. upstream
/// 3.5.1 fails the entry's `link_stat` with `ELOOP` (exit 23).
#[cfg(unix)]
#[test]
fn files_from_file_through_a_link_leaving_the_confine_root_is_refused() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(Some(&fx.root));
    let (dest, ok) = fx.transfer("outside-link/marker", "dest-file", true);
    assert!(!ok, "the out-of-root entry must fail the transfer");
    assert!(
        !dest.join("outside-link/marker").exists(),
        "out-of-root content from {} was copied",
        fx.outside.display()
    );
}

/// WITNESS. A directory entry through the same link is refused before it is
/// enumerated.
#[cfg(unix)]
#[test]
fn files_from_dir_through_a_link_leaving_the_confine_root_is_refused() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(Some(&fx.root));
    let (dest, ok) = fx.transfer("outside-link/", "dest-dir", true);
    assert!(
        !ok,
        "the out-of-root directory entry must fail the transfer"
    );
    assert!(!dest.join("outside-link/marker").exists());
}

/// WITNESS. A link to the root's own PARENT resolves to an ancestor of the
/// root. Upstream 3.5.1 allows an ancestor only while descending
/// (`abspath_outside_confinement(abspath, final)`), so an entry that ENDS
/// there names the tree above the root and is refused.
#[cfg(unix)]
#[test]
fn files_from_link_to_an_ancestor_of_the_confine_root_is_refused() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(Some(&fx.root));
    let (dest, ok) = fx.transfer("ancestor-link/", "dest-ancestor", false);
    assert!(
        !ok,
        "an entry resolving above the root must fail the transfer"
    );
    assert!(!dest.join("ancestor-link").exists());
}

/// CONTROL. A trusted link that stays inside the root is still followed, so
/// the refusals above are about WHERE the entry lands, not about links.
#[cfg(unix)]
#[test]
fn files_from_link_inside_the_confine_root_is_followed() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(Some(&fx.root));
    let (dest, ok) = fx.transfer("inside-link/marker", "dest-inside", true);
    assert!(ok, "an in-root entry must transfer");
    assert_eq!(
        fs::read(dest.join("inside-link/marker")).expect("copied"),
        b"inside contents"
    );
}

/// NON-VACUITY COMPANION. Without a confinement root the same trusted link is
/// the operator's own layout and upstream follows it, so the fixture really
/// does reach the outside content when nothing confines it.
#[cfg(unix)]
#[test]
fn files_from_link_leaving_the_tree_is_followed_without_a_confine_root() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(None);
    let (dest, ok) = fx.transfer("outside-link/marker", "dest-unconfined", true);
    assert!(ok, "an unconfined trusted link must be followed");
    assert_eq!(
        fs::read(dest.join("outside-link/marker")).expect("copied"),
        b"outside contents"
    );
}

/// CONTROL. An entry carrying its own `/./` marker names its base directory
/// itself (`inside/./`): the walk resolves the base as the leaf `.` of an
/// empty parent, as upstream's `strrchr()` split does, and transfers the
/// directory's contents under the destination root.
#[cfg(unix)]
#[test]
fn files_from_entry_naming_its_own_base_transfers_the_contents() {
    let fx = FilesFromConfinementFixture::new();
    let _session = install_backup_confinement(Some(&fx.root));
    let (dest, ok) = fx.transfer("inside/./", "dest-dot", true);
    assert!(ok, "a dot-anchored entry must transfer");
    assert_eq!(
        fs::read(dest.join("marker")).expect("copied"),
        b"inside contents"
    );
}
