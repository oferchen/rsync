// Tests for file comparison and skip logic in executor.

#[cfg(test)]
mod file_comparison_tests {
    use super::*;

    #[test]
    fn should_skip_copy_different_sizes() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"longer content").expect("write source");
        fs::write(&dest, b"short").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(!should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_same_size_same_time() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&dest, b"content").expect("write dest");

        // Set same mtime
        let source_meta = fs::metadata(&source).expect("source metadata");
        let mtime = FileTime::from_system_time(source_meta.modified().expect("source mtime"));
        set_file_mtime(&dest, mtime).expect("set dest mtime");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_same_size_different_time_within_window() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&dest, b"content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        // Use a large modify window
        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::from_secs(3600), // 1 hour window
            prefetched_match: None,
        };

        // Should skip because times are within window
        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_size_only_mode() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        // Same size, different content
        fs::write(&source, b"aaaaaaa").expect("write source");
        fs::write(&dest, b"bbbbbbb").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: true,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_ignore_times_forces_copy() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&dest, b"content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: true, // Even with size_only
            ignore_times: true,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(!should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_checksum_mode_identical_content() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"identical content").expect("write source");
        fs::write(&dest, b"identical content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_checksum_mode_different_content() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        // Same size, different content
        fs::write(&source, b"aaaaaaa").expect("write source");
        fs::write(&dest, b"bbbbbbb").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(!should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_prefetched_match_true() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&dest, b"content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: Some(true), // Prefetched indicates match
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_prefetched_match_false() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&dest, b"content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: SignatureAlgorithm::Md4,
            modify_window: Duration::ZERO,
            prefetched_match: Some(false), // Prefetched indicates no match
        };

        assert!(!should_skip_copy(comparison));
    }

    #[test]
    fn files_checksum_match_identical_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"identical content here").expect("write source");
        fs::write(&dest, b"identical content here").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_different_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source content").expect("write source");
        fs::write(&dest, b"dest content!!").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn files_checksum_match_md5_algorithm() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let result = files_checksum_match(
            &source,
            &dest,
            SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::none(),
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_sha1_algorithm() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Sha1);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_xxh64_algorithm() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Xxh64 { seed: 0 });
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_xxh3_algorithm() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Xxh3 { seed: 0 });
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_xxh128_algorithm() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Xxh3_128 { seed: 0 });
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_large_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let dest = temp.path().join("dest.bin");

        // Create large identical files (>64KB to test buffering)
        let large_data = vec![0xABu8; 100_000];
        fs::write(&source, &large_data).expect("write source");
        fs::write(&dest, &large_data).expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_large_files_differ_at_end() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let dest = temp.path().join("dest.bin");

        let source_data = vec![0xABu8; 100_000];
        let mut dest_data = source_data.clone();
        dest_data[99_999] = 0xCD; // Differ at the very end

        fs::write(&source, &source_data).expect("write source");
        fs::write(&dest, &dest_data).expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn files_checksum_match_empty_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"").expect("write source");
        fs::write(&dest, b"").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn files_checksum_match_nonexistent_source() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("nonexistent.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&dest, b"content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_err());
    }

    #[test]
    fn files_checksum_match_nonexistent_destination() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("nonexistent.txt");

        fs::write(&source, b"content").expect("write source");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_err());
    }

    #[test]
    fn files_checksum_match_different_sizes_early_exit() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"short").expect("write source");
        fs::write(&dest, b"much longer content").expect("write dest");

        let result = files_checksum_match(&source, &dest, SignatureAlgorithm::Md4);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn should_skip_copy_with_md5_seed() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"test content").expect("write source");
        fs::write(&dest, b"test content").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta = fs::metadata(&dest).expect("dest metadata");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &dest,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::proper(42),
            },
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }
}
