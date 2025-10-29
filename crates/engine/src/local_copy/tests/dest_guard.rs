
#[test]
fn destination_write_guard_uses_custom_partial_directory() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let partial_dir = Path::new(".custom-partial");

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), true, Some(partial_dir), None)
            .expect("guard");
    let temp_path = guard.temp_path.clone();
    file.write_all(b"partial payload").expect("write partial");
    drop(file);

    drop(guard);

    let expected_base = destination_dir.join(partial_dir);
    assert!(temp_path.starts_with(&expected_base));
    assert!(temp_path.exists());
    assert!(!destination.exists());
}

#[test]
fn destination_write_guard_commit_moves_from_partial_directory() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let partial_dir = temp.path().join("partials");

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(partial_dir.as_path()),
        None,
    )
    .expect("guard");
    let temp_path = guard.temp_path.clone();
    file.write_all(b"committed payload").expect("write payload");
    drop(file);

    guard.commit().expect("commit succeeds");

    assert!(!temp_path.exists());
    let committed = fs::read(&destination).expect("read committed file");
    assert_eq!(committed, b"committed payload");
}

#[test]
fn destination_write_guard_uses_custom_temp_directory_for_non_partial() {
    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    let destination = destination_dir.join("file.txt");
    let temp_dir = temp.path().join("tmp-area");
    fs::create_dir_all(&temp_dir).expect("temp dir");

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), false, None, Some(temp_dir.as_path()))
            .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"temporary payload").expect("write temp");
    drop(file);

    guard.commit().expect("commit");

    assert!(staging_path.starts_with(&temp_dir));
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"temporary payload"
    );
    assert!(!staging_path.exists());
}
