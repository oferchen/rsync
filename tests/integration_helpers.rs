//! Integration test helper infrastructure tests.
//!
//! Validates that the test utilities work correctly.

mod integration;

use integration::helpers::*;

#[test]
fn test_dir_creates_and_cleans_up() {
    let path_copy;
    {
        let test_dir = TestDir::new().expect("create test dir");
        path_copy = test_dir.path().to_path_buf();
        assert!(path_copy.exists(), "test dir should exist");
    }
    assert!(!path_copy.exists(), "test dir should be cleaned up");
}

#[test]
fn test_dir_write_and_read() {
    let test_dir = TestDir::new().expect("create test dir");
    test_dir
        .write_file("test.txt", b"hello")
        .expect("write file");
    let content = test_dir.read_file("test.txt").expect("read file");
    assert_eq!(content, b"hello");
}

#[test]
fn test_file_tree_creates_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let mut tree = FileTree::new();
    tree.text_file("a.txt", "content a");
    tree.text_file("subdir/b.txt", "content b");
    tree.create_in(&test_dir).expect("create tree");

    assert!(test_dir.exists("a.txt"));
    assert!(test_dir.exists("subdir/b.txt"));
    assert_eq!(test_dir.read_file("a.txt").unwrap(), b"content a");
}

#[test]
fn rsync_command_locates_binary() {
    // This test just verifies the binary can be found
    let _cmd = RsyncCommand::new();
}
