// The copy a `--copy-dest` match makes re-opens the alt file after
// `basis_stat` chose it. rsync 3.5.2 opens it through the ownership walk with
// O_NOFOLLOW (util1.c:414-428), so a name swapped between the check and the
// open is refused instead of read; the file then transfers from the source and
// the run ends with 23 (generator.c:1209-1213, :2197-2207).

/// Source, alt file and a file outside both, all the same size, with source
/// and alt file sharing an mtime so the quick check picks the alt file.
struct AltBasisFixture {
    _temp: tempfile::TempDir,
    source_file: PathBuf,
    alt_dir: PathBuf,
    outside_file: PathBuf,
    destination_file: PathBuf,
}

fn alt_basis_fixture(alt_dir_is_symlink: bool) -> AltBasisFixture {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let real_alt_dir = temp.path().join("real-alt");
    let alt_dir = temp.path().join("alt");
    let destination_dir = temp.path().join("dest");
    for dir in [&source_dir, &real_alt_dir, &destination_dir] {
        fs::create_dir_all(dir).expect("create dir");
    }
    if alt_dir_is_symlink {
        std::os::unix::fs::symlink(&real_alt_dir, &alt_dir).expect("alt dir symlink");
    } else {
        fs::rename(&real_alt_dir, &alt_dir).expect("place alt dir");
    }

    let source_file = source_dir.join("f");
    let alt_file = alt_dir.join("f");
    let outside_file = temp.path().join("outside");
    fs::write(&source_file, b"SRCDATA\n").expect("write source");
    fs::write(&alt_file, b"ALTDATA\n").expect("write alt");
    fs::write(&outside_file, b"OUTDATA\n").expect("write outside");
    let timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&alt_file, timestamp).expect("alt mtime");

    AltBasisFixture {
        _temp: temp,
        source_file,
        alt_dir,
        outside_file,
        destination_file: destination_dir.join("f"),
    }
}

fn run_copy_dest(
    fixture: &AltBasisFixture,
    copy_links: bool,
) -> Result<LocalCopySummary, LocalCopyError> {
    let operands = vec![
        fixture.source_file.clone().into_os_string(),
        fixture.destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .copy_links(copy_links)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &fixture.alt_dir,
        )]);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
}

#[test]
fn copy_dest_alt_file_swapped_to_a_symlink_after_the_check_is_not_read() {
    for copy_links in [false, true] {
        let case = format!("copy_links={copy_links}");
        let fixture = alt_basis_fixture(false);
        let alt_file = fixture.alt_dir.join("f");
        let outside = fixture.outside_file.clone();
        let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&swapped);

        let result = with_alt_basis_open_hook(
            move |path| {
                assert_eq!(path, alt_file.as_path(), "opened a different alt file");
                fs::remove_file(&alt_file).expect("remove checked alt file");
                std::os::unix::fs::symlink(&outside, &alt_file).expect("plant symlink");
                seen.set(true);
            },
            || run_copy_dest(&fixture, copy_links),
        );

        assert!(swapped.get(), "{case}: the alt file was never opened");
        let error = result.expect_err("a refused alt open must end the run with 23");
        assert_eq!(error.exit_code(), 23, "{case}: exit code");
        assert_eq!(
            fs::read(&fixture.destination_file).expect("destination written"),
            b"SRCDATA\n",
            "{case}: the file must transfer from the source, never through the planted link"
        );
    }
}

#[test]
fn copy_dest_alt_file_left_in_place_is_copied_through_the_alt_opener() {
    let fixture = alt_basis_fixture(false);
    let opened = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let count = std::rc::Rc::clone(&opened);

    let summary = with_alt_basis_open_hook(
        move |_| count.set(count.get() + 1),
        || run_copy_dest(&fixture, false),
    )
    .expect("copy succeeds");

    assert_eq!(opened.get(), 1, "the alt file is opened once");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&fixture.destination_file).expect("read dest"),
        b"ALTDATA\n",
        "a --copy-dest match copies the alt file's bytes"
    );
}

#[test]
fn copy_dest_through_a_self_owned_alt_dir_symlink_is_followed() {
    // rsync 3.5.2 follows an alt directory that is a symlink we own, for both a
    // relative and an absolute --copy-dest: the ownership walk trusts the euid.
    let fixture = alt_basis_fixture(true);

    run_copy_dest(&fixture, false).expect("copy succeeds");

    assert_eq!(
        fs::read(&fixture.destination_file).expect("read dest"),
        b"ALTDATA\n"
    );
}

#[test]
fn copy_dest_alt_dir_swapped_after_the_check_does_not_redirect_the_open() {
    // The copy opens the leaf beneath the directory the check walked. Renaming
    // that directory away and planting a symlink at its name, even one the
    // operator owns, cannot point the open at another directory.
    let fixture = alt_basis_fixture(false);
    let outside_dir = fixture.outside_file.with_extension("dir");
    fs::create_dir(&outside_dir).expect("outside dir");
    fs::write(outside_dir.join("f"), b"OUTDATA\n").expect("outside file");
    let alt_dir = fixture.alt_dir.clone();
    let moved = fixture.alt_dir.with_extension("checked");

    with_alt_basis_open_hook(
        move |_| {
            fs::rename(&alt_dir, &moved).expect("move checked alt dir away");
            std::os::unix::fs::symlink(&outside_dir, &alt_dir).expect("plant dir symlink");
        },
        || run_copy_dest(&fixture, false),
    )
    .expect("the checked alt file is still copied");

    assert_eq!(
        fs::read(&fixture.destination_file).expect("read dest"),
        b"ALTDATA\n",
        "the bytes must come from the directory the check walked"
    );
}

#[test]
fn copy_dest_keeps_one_walked_parent_however_many_candidates_it_checks() {
    // Every candidate is a match-level-2 file (same size and mtime, different
    // mode under -p), so the scan visits all of them. Only the best keeps its
    // walked parent; the others' descriptors must be closed before the open.
    const CANDIDATES: usize = 300;
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("source dir");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let source_file = source_dir.join("f");
    fs::write(&source_file, b"SRCDATA\n").expect("write source");
    let timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");

    let mut references = Vec::with_capacity(CANDIDATES);
    for index in 0..CANDIDATES {
        let dir = temp.path().join(format!("alt{index}"));
        fs::create_dir(&dir).expect("alt dir");
        let file = dir.join("f");
        fs::write(&file, b"ALTDATA\n").expect("write alt");
        set_file_mtime(&file, timestamp).expect("alt mtime");
        fs::set_permissions(
            &file,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .expect("alt mode");
        references.push(ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &dir));
    }

    let open_descriptors = || fs::read_dir("/dev/fd").expect("list /dev/fd").count();
    let before = open_descriptors();
    let at_open = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = std::rc::Rc::clone(&at_open);

    let destination_file = destination_dir.join("f");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .permissions(true)
        .extend_reference_directories(references);
    with_alt_basis_open_hook(
        move |_| seen.set(open_descriptors()),
        || plan.execute_with_options(LocalCopyExecution::Apply, options),
    )
    .expect("copy succeeds");

    assert_eq!(
        fs::read(&destination_file).expect("read dest"),
        b"ALTDATA\n"
    );
    let held = at_open.get().saturating_sub(before);
    assert!(
        held < 16,
        "{held} more descriptors were open at the alt open than before the run; \
         a walked parent kept per candidate would leave about {CANDIDATES}"
    );
}
