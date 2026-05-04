// Tests for file operation helpers in executor.

#[cfg(test)]
mod file_operations_tests {
    use super::*;

    #[test]
    fn remove_existing_destination_removes_file() {
        let temp = tempdir().expect("tempdir");
        let file = temp.path().join("file.txt");
        fs::write(&file, b"content").expect("write");

        let result = remove_existing_destination(&file);
        assert!(result.is_ok());
        assert!(!file.exists());
    }

    #[test]
    fn remove_existing_destination_handles_nonexistent() {
        let temp = tempdir().expect("tempdir");
        let file = temp.path().join("nonexistent.txt");

        let result = remove_existing_destination(&file);
        assert!(result.is_ok());
    }

    #[test]
    fn compute_backup_path_with_suffix() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("file.txt");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            None,
            None,
            OsStr::new("~"),
        );

        assert_eq!(backup, dest_root.join("file.txt~"));
    }

    #[test]
    fn compute_backup_path_with_backup_dir() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("file.txt");
        let backup_dir = temp.path().join("backups");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            None,
            Some(&backup_dir),
            OsStr::new("~"),
        );

        assert_eq!(backup, backup_dir.join("file.txt~"));
    }

    #[test]
    fn compute_backup_path_preserves_directory_structure() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("subdir").join("file.txt");
        let backup_dir = temp.path().join("backups");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            Some(Path::new("subdir/file.txt")),
            Some(&backup_dir),
            OsStr::new(".bak"),
        );

        assert_eq!(backup, backup_dir.join("subdir").join("file.txt.bak"));
    }

    #[test]
    fn compute_backup_path_empty_suffix() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("file.txt");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            None,
            None,
            OsStr::new(""),
        );

        assert_eq!(backup, dest_root.join("file.txt"));
    }

    #[test]
    fn copy_entry_to_backup_file() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let backup = temp.path().join("backup.txt");

        fs::write(&source, b"original content").expect("write");

        let metadata = fs::metadata(&source).expect("metadata");
        let result = copy_entry_to_backup(&source, &backup, metadata.file_type());

        assert!(result.is_ok());
        assert!(backup.exists());
        assert_eq!(
            fs::read_to_string(&backup).expect("read"),
            "original content"
        );
    }

    #[test]
    fn copy_entry_to_backup_symlink() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        let symlink = temp.path().join("link.txt");
        let backup = temp.path().join("backup_link.txt");

        fs::write(&target, b"target content").expect("write target");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlink).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &symlink).expect("symlink");

        let metadata = fs::symlink_metadata(&symlink).expect("metadata");
        let result = copy_entry_to_backup(&symlink, &backup, metadata.file_type());

        assert!(result.is_ok());
        assert!(backup.exists());

        // Backup should be a symlink to the same target
        let backup_meta = fs::symlink_metadata(&backup).expect("backup metadata");
        assert!(backup_meta.is_symlink());
    }

    #[test]
    fn copy_entry_to_backup_directory_ignored() {
        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("source_dir");
        let backup = temp.path().join("backup_dir");

        fs::create_dir(&source_dir).expect("mkdir");
        fs::write(source_dir.join("file.txt"), b"content").expect("write");

        let metadata = fs::metadata(&source_dir).expect("metadata");
        let result = copy_entry_to_backup(&source_dir, &backup, metadata.file_type());

        // Should succeed but not copy directory
        assert!(result.is_ok());
        assert!(!backup.exists());
    }

    #[test]
    fn partial_destination_path_adds_prefix() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);

        let partial_str = partial.to_string_lossy();
        assert!(partial_str.contains(".rsync-partial-"));
        assert!(partial_str.contains("file.txt"));
    }

    #[test]
    fn partial_destination_path_same_directory() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);

        assert_eq!(partial.parent(), dest.parent());
    }

    #[test]
    fn partial_destination_path_no_filename() {
        let dest = Path::new("/");
        let partial = partial_destination_path(dest);

        assert!(partial.to_string_lossy().contains("partial"));
    }

    #[test]
    fn copy_entry_to_backup_missing_source() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("nonexistent.txt");
        let backup = temp.path().join("backup.txt");

        // Create dummy metadata - this will fail when trying to copy
        let dummy_file = temp.path().join("dummy.txt");
        fs::write(&dummy_file, b"dummy").expect("write");
        let metadata = fs::metadata(&dummy_file).expect("metadata");

        let result = copy_entry_to_backup(&source, &backup, metadata.file_type());

        // Should fail because source doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn copy_entry_to_backup_to_readonly_location() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"content").expect("write");

        // Try to backup to a location that doesn't exist and can't be created
        let backup = Path::new("/nonexistent/impossible/backup.txt");

        let metadata = fs::metadata(&source).expect("metadata");
        let result = copy_entry_to_backup(&source, backup, metadata.file_type());

        // Should fail because backup location is invalid
        assert!(result.is_err());
    }

    #[test]
    fn compute_backup_path_relative_backup_dir() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("file.txt");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            None,
            Some(Path::new("backups")),
            OsStr::new("~"),
        );

        assert_eq!(backup, dest_root.join("backups").join("file.txt~"));
    }

    #[test]
    fn compute_backup_path_nested_relative_path() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("a").join("b").join("c.txt");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            Some(Path::new("a/b/c.txt")),
            None,
            OsStr::new(".old"),
        );

        assert_eq!(backup, dest_root.join("a").join("b").join("c.txt.old"));
    }

    #[test]
    fn compute_backup_path_destination_outside_root() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().join("root");
        let dest = temp.path().join("outside").join("file.txt");

        let backup = compute_backup_path(
            &dest_root,
            &dest,
            None,
            None,
            OsStr::new("~"),
        );

        // Should use dest's parent when dest is not under dest_root
        assert_eq!(backup, temp.path().join("outside").join("file.txt~"));
    }

    #[test]
    fn compute_backup_path_with_dotdot_in_relative() {
        let temp = tempdir().expect("tempdir");
        let dest_root = temp.path().to_path_buf();
        let dest = dest_root.join("subdir").join("file.txt");

        // This is unusual but should be handled
        let backup = compute_backup_path(
            &dest_root,
            &dest,
            Some(Path::new("subdir/file.txt")),
            Some(Path::new("../backups")),
            OsStr::new("~"),
        );

        // dest_root.join("../backups") should work
        let expected = dest_root.join("../backups").join("subdir").join("file.txt~");
        assert_eq!(backup, expected);
    }

    #[test]
    fn copy_entry_to_backup_overwrites_existing() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let backup = temp.path().join("backup.txt");

        fs::write(&source, b"new content").expect("write source");
        fs::write(&backup, b"old backup").expect("write old backup");

        let metadata = fs::metadata(&source).expect("metadata");
        let result = copy_entry_to_backup(&source, &backup, metadata.file_type());

        assert!(result.is_ok());
        assert_eq!(
            fs::read_to_string(&backup).expect("read"),
            "new content"
        );
    }

    #[test]
    fn copy_entry_to_backup_broken_symlink() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("nonexistent_target.txt");
        let symlink = temp.path().join("broken_link.txt");
        let backup = temp.path().join("backup_link.txt");

        // Create broken symlink
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlink).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &symlink).expect("symlink");

        let metadata = fs::symlink_metadata(&symlink).expect("metadata");
        let result = copy_entry_to_backup(&symlink, &backup, metadata.file_type());

        assert!(result.is_ok());

        // Backup should also be a broken symlink
        let backup_meta = fs::symlink_metadata(&backup).expect("backup metadata");
        assert!(backup_meta.is_symlink());

        // The target should be the same (even though it doesn't exist)
        assert_eq!(
            fs::read_link(&backup).expect("read backup link"),
            target
        );
    }
}
