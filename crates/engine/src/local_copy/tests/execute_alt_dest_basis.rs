// A `--compare-dest` / `--copy-dest` / `--link-dest` file that differs from the
// source is the delta basis for a destination that does not exist yet.
//
// upstream: generator.c:1119-1126 (the first basis dir holding a regular file is
// match_level 1), :1230 (`return FNAMECMP_BASIS_DIR_LOW + j`), :2201-2205 (it
// becomes `fnamecmp`); receiver.c:1094-1103 reads matched blocks from it.

/// The basis holds the source's bytes from offset 100 on, so a delta against it
/// matches most of the file while a whole-file copy matches nothing.
struct AltDestFixture {
    _temp: tempfile::TempDir,
    source_file: PathBuf,
    dest_file: PathBuf,
    alt_dirs: Vec<PathBuf>,
    source_content: Vec<u8>,
}

fn alt_dest_fixture(alt_contents: &[Option<Vec<u8>>]) -> AltDestFixture {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_content = noise_bytes(11, 48 * 1024);
    let source_file = source_dir.join("payload.bin");
    fs::write(&source_file, &source_content).expect("write source");
    set_file_mtime(&source_file, FileTime::from_unix_time(1_700_000_000, 0)).expect("stamp source");

    let mut alt_dirs = Vec::new();
    for (index, content) in alt_contents.iter().enumerate() {
        let dir = temp.path().join(format!("alt{index}"));
        fs::create_dir_all(&dir).expect("create alt");
        if let Some(content) = content {
            let file = dir.join("payload.bin");
            fs::write(&file, content).expect("write alt");
            set_file_mtime(&file, FileTime::from_unix_time(1_600_000_000, 0)).expect("stamp alt");
        }
        alt_dirs.push(dir);
    }

    AltDestFixture {
        dest_file: dest_dir.join("payload.bin"),
        _temp: temp,
        source_file,
        alt_dirs,
        source_content,
    }
}

fn shifted_alt_basis(source: &[u8]) -> Vec<u8> {
    source[100..100 + 32 * 1024].to_vec()
}

fn run_alt_dest(
    fixture: &AltDestFixture,
    kind: ReferenceDirectoryKind,
    whole_file: bool,
) -> LocalCopySummary {
    let operands = vec![
        fixture.source_file.clone().into_os_string(),
        fixture.dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .whole_file(whole_file)
        .extend_reference_directories(
            fixture
                .alt_dirs
                .iter()
                .map(|dir| ReferenceDirectory::new(kind, dir)),
        );
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds")
}

#[test]
fn alt_dest_differing_file_is_the_delta_basis_for_a_missing_destination() {
    for kind in [
        ReferenceDirectoryKind::Compare,
        ReferenceDirectoryKind::Copy,
        ReferenceDirectoryKind::Link,
    ] {
        let probe = noise_bytes(11, 48 * 1024);
        let fixture = alt_dest_fixture(&[Some(shifted_alt_basis(&probe))]);
        let alt_file = fixture.alt_dirs[0].join("payload.bin");
        let alt_before = fs::read(&alt_file).expect("read alt");

        let summary = run_alt_dest(&fixture, kind, false);

        assert_eq!(
            fs::read(&fixture.dest_file).expect("read dest"),
            fixture.source_content,
            "{kind:?}: destination must equal the source"
        );
        assert!(
            summary.matched_bytes() >= 24 * 1024,
            "{kind:?}: blocks of the alt file must match, matched {} bytes",
            summary.matched_bytes()
        );
        assert_eq!(
            fs::read(&alt_file).expect("read alt"),
            alt_before,
            "{kind:?}: the basis is only read"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                fs::metadata(&fixture.dest_file).expect("stat dest").ino(),
                fs::metadata(&alt_file).expect("stat alt").ino(),
                "{kind:?}: a differing basis is never hard-linked"
            );
        }
    }
}

// The default local copy is whole-file: nothing is read from the alt file and
// every byte is literal, exactly as before.
#[test]
fn alt_dest_differing_file_is_ignored_by_a_whole_file_copy() {
    let probe = noise_bytes(11, 48 * 1024);
    let fixture = alt_dest_fixture(&[Some(shifted_alt_basis(&probe))]);

    let summary = run_alt_dest(&fixture, ReferenceDirectoryKind::Compare, true);

    assert_eq!(
        fs::read(&fixture.dest_file).expect("read dest"),
        fixture.source_content
    );
    assert_eq!(summary.matched_bytes(), 0);
    assert_eq!(summary.bytes_copied(), fixture.source_content.len() as u64);
}

// upstream: generator.c:2196 runs try_dests_reg with find_exact_for_existing
// for an existing destination, and :1195-1196 drops anything below an exact
// match, so the destination stays the basis.
#[test]
fn alt_dest_differing_file_never_replaces_an_existing_destination_as_basis() {
    let probe = noise_bytes(11, 48 * 1024);
    let fixture = alt_dest_fixture(&[Some(shifted_alt_basis(&probe))]);
    // An unrelated destination: a delta against it matches nothing.
    fs::write(&fixture.dest_file, noise_bytes(99, 32 * 1024)).expect("write dest");
    set_file_mtime(
        &fixture.dest_file,
        FileTime::from_unix_time(1_500_000_000, 0),
    )
    .expect("stamp dest");

    let summary = run_alt_dest(&fixture, ReferenceDirectoryKind::Compare, false);

    assert_eq!(
        fs::read(&fixture.dest_file).expect("read dest"),
        fixture.source_content
    );
    assert_eq!(
        summary.matched_bytes(),
        0,
        "the existing destination, not the alt file, is the basis"
    );
}

// upstream: generator.c:1123-1126 - match_level 1 is set once, by the first
// basis dir holding a regular file, so dir order decides.
#[test]
fn alt_dest_first_listed_differing_file_is_the_basis() {
    let probe = noise_bytes(11, 48 * 1024);
    let unrelated = noise_bytes(77, 32 * 1024);

    let first_useful =
        alt_dest_fixture(&[Some(shifted_alt_basis(&probe)), Some(unrelated.clone())]);
    let summary = run_alt_dest(&first_useful, ReferenceDirectoryKind::Compare, false);
    assert!(summary.matched_bytes() >= 24 * 1024);

    let first_unrelated = alt_dest_fixture(&[Some(unrelated), Some(shifted_alt_basis(&probe))]);
    let summary = run_alt_dest(&first_unrelated, ReferenceDirectoryKind::Compare, false);
    assert_eq!(
        summary.matched_bytes(),
        0,
        "the unrelated first dir is the basis, not the useful second one"
    );
    assert_eq!(
        fs::read(&first_unrelated.dest_file).expect("read dest"),
        first_unrelated.source_content
    );
}

// A dir without the name is skipped, so a later dir's file is the basis.
#[test]
fn alt_dest_basis_skips_a_dir_without_the_file() {
    let probe = noise_bytes(11, 48 * 1024);
    let fixture = alt_dest_fixture(&[None, Some(shifted_alt_basis(&probe))]);

    let summary = run_alt_dest(&fixture, ReferenceDirectoryKind::Copy, false);

    assert!(summary.matched_bytes() >= 24 * 1024);
}

// upstream: generator.c:1121 skips a basis entry that is not S_ISREG under
// lstat, so a symlink leaving the alt dir is never read as the basis.
#[cfg(unix)]
#[test]
fn alt_dest_symlinked_file_is_not_a_basis() {
    let probe = noise_bytes(11, 48 * 1024);
    let fixture = alt_dest_fixture(&[None]);
    let outside = fixture.alt_dirs[0].parent().unwrap().join("outside.bin");
    fs::write(&outside, shifted_alt_basis(&probe)).expect("write outside");
    std::os::unix::fs::symlink(&outside, fixture.alt_dirs[0].join("payload.bin"))
        .expect("plant symlink");

    let summary = run_alt_dest(&fixture, ReferenceDirectoryKind::Compare, false);

    assert_eq!(summary.matched_bytes(), 0);
    assert_eq!(
        fs::read(&fixture.dest_file).expect("read dest"),
        fixture.source_content
    );
}
