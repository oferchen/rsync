use super::prelude::*;


#[test]
fn resolve_destination_path_returns_existing_candidate() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("dest");
    fs::create_dir_all(&root).expect("create dest root");
    let subdir = root.join("sub");
    fs::create_dir_all(&subdir).expect("create subdir");
    let file_path = subdir.join("file.txt");
    fs::write(&file_path, b"payload").expect("write file");

    let relative = Path::new("sub").join("file.txt");
    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative.as_path());

    assert_eq!(resolved, file_path);
}


#[test]
fn resolve_destination_path_returns_root_for_file_destination() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("target.bin");
    let relative = Path::new("target.bin");

    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

    assert_eq!(resolved, root);
}


#[test]
fn resolve_destination_path_preserves_missing_entries() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("dest");
    fs::create_dir_all(&root).expect("create destination root");
    let relative = Path::new("missing.bin");

    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

    assert_eq!(resolved, root.join(relative));
}

