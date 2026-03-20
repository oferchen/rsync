use std::num::NonZeroU64;
use std::time::{Duration, SystemTime};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;

use super::*;
use crate::local_copy::options::types::{DeleteTiming, LocalCopyOptions};

mod builder_creation {
    use super::*;

    #[test]
    fn new_creates_builder_with_defaults() {
        let builder = LocalCopyOptionsBuilder::new();
        let options = builder.build().expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.whole_file_enabled());
        assert!(options.implied_dirs_enabled());
        assert!(!options.delete_extraneous());
        assert!(!options.compress_enabled());
    }

    #[test]
    fn default_trait_matches_new() {
        let builder1 = LocalCopyOptionsBuilder::new();
        let builder2 = LocalCopyOptionsBuilder::default();

        let options1 = builder1.build().expect("valid options");
        let options2 = builder2.build().expect("valid options");

        assert_eq!(options1.recursive_enabled(), options2.recursive_enabled());
        assert_eq!(options1.delete_extraneous(), options2.delete_extraneous());
    }

    #[test]
    fn builder_method_on_local_copy_options() {
        let options = LocalCopyOptions::builder().build().expect("valid options");
        assert!(options.recursive_enabled());
    }
}

mod presets {
    use super::*;

    #[test]
    fn archive_preset_enables_expected_options() {
        let options = LocalCopyOptionsBuilder::new()
            .archive()
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.links_enabled());
        assert!(options.preserve_permissions());
        assert!(options.preserve_times());
        assert!(options.preserve_group());
        assert!(options.preserve_owner());
        assert!(options.devices_enabled());
        assert!(options.specials_enabled());
    }

    #[test]
    fn sync_preset_enables_archive_and_delete() {
        let options = LocalCopyOptionsBuilder::new()
            .sync()
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.delete_extraneous());
    }

    #[test]
    fn backup_preset_enables_archive_and_extras() {
        let options = LocalCopyOptionsBuilder::new()
            .backup_preset()
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.hard_links_enabled());
        assert!(options.partial_enabled());
    }
}

mod deletion_options {
    use super::*;

    #[test]
    fn delete_enables_deletion() {
        let options = LocalCopyOptionsBuilder::new()
            .delete(true)
            .build()
            .expect("valid options");

        assert!(options.delete_extraneous());
        assert_eq!(options.delete_timing(), Some(DeleteTiming::During));
    }

    #[test]
    fn delete_before_sets_timing() {
        let options = LocalCopyOptionsBuilder::new()
            .delete_before(true)
            .build()
            .expect("valid options");

        assert!(options.delete_extraneous());
        assert_eq!(options.delete_timing(), Some(DeleteTiming::Before));
    }

    #[test]
    fn delete_after_sets_timing() {
        let options = LocalCopyOptionsBuilder::new()
            .delete_after(true)
            .build()
            .expect("valid options");

        assert!(options.delete_extraneous());
        assert_eq!(options.delete_timing(), Some(DeleteTiming::After));
    }

    #[test]
    fn delete_delay_sets_timing() {
        let options = LocalCopyOptionsBuilder::new()
            .delete_delay(true)
            .build()
            .expect("valid options");

        assert!(options.delete_extraneous());
        assert_eq!(options.delete_timing(), Some(DeleteTiming::Delay));
    }

    #[test]
    fn max_deletions_sets_limit() {
        let options = LocalCopyOptionsBuilder::new()
            .max_deletions(Some(100))
            .build()
            .expect("valid options");

        assert_eq!(options.max_deletion_limit(), Some(100));
    }
}

mod size_limits {
    use super::*;

    #[test]
    fn min_file_size_sets_limit() {
        let options = LocalCopyOptionsBuilder::new()
            .min_file_size(Some(1024))
            .build()
            .expect("valid options");

        assert_eq!(options.min_file_size_limit(), Some(1024));
    }

    #[test]
    fn max_file_size_sets_limit() {
        let options = LocalCopyOptionsBuilder::new()
            .max_file_size(Some(1_000_000))
            .build()
            .expect("valid options");

        assert_eq!(options.max_file_size_limit(), Some(1_000_000));
    }
}

mod transfer_options {
    use super::*;

    #[test]
    fn remove_source_files_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .remove_source_files(true)
            .build()
            .expect("valid options");

        assert!(options.remove_source_files_enabled());
    }

    #[test]
    fn preallocate_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .preallocate(true)
            .build()
            .expect("valid options");

        assert!(options.preallocate_enabled());
    }

    #[test]
    fn fsync_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .fsync(true)
            .build()
            .expect("valid options");

        assert!(options.fsync_enabled());
    }

    #[test]
    fn bandwidth_limit_sets_value() {
        let limit = NonZeroU64::new(1_000_000).unwrap();
        let options = LocalCopyOptionsBuilder::new()
            .bandwidth_limit(Some(limit))
            .build()
            .expect("valid options");

        assert_eq!(options.bandwidth_limit_bytes(), Some(limit));
    }
}

mod compression_options {
    use super::*;

    #[test]
    fn compress_enables_compression() {
        let options = LocalCopyOptionsBuilder::new()
            .compress(true)
            .build()
            .expect("valid options");

        assert!(options.compress_enabled());
    }

    #[test]
    fn compression_algorithm_sets_value() {
        let options = LocalCopyOptionsBuilder::new()
            .compression_algorithm(CompressionAlgorithm::Zstd)
            .build()
            .expect("valid options");

        assert_eq!(options.compression_algorithm(), CompressionAlgorithm::Zstd);
    }

    #[test]
    fn compression_level_sets_value() {
        let options = LocalCopyOptionsBuilder::new()
            .compression_level(CompressionLevel::Best)
            .build()
            .expect("valid options");

        assert_eq!(options.compression_level(), CompressionLevel::Best);
    }
}

mod metadata_options {
    use super::*;

    #[test]
    fn owner_preservation() {
        let options = LocalCopyOptionsBuilder::new()
            .preserve_owner(true)
            .build()
            .expect("valid options");

        assert!(options.preserve_owner());
    }

    #[test]
    fn group_preservation() {
        let options = LocalCopyOptionsBuilder::new()
            .preserve_group(true)
            .build()
            .expect("valid options");

        assert!(options.preserve_group());
    }

    #[test]
    fn permissions_preservation() {
        let options = LocalCopyOptionsBuilder::new()
            .preserve_permissions(true)
            .build()
            .expect("valid options");

        assert!(options.preserve_permissions());
    }

    #[test]
    fn times_preservation() {
        let options = LocalCopyOptionsBuilder::new()
            .preserve_times(true)
            .build()
            .expect("valid options");

        assert!(options.preserve_times());
    }

    #[test]
    fn alias_methods_work() {
        let options = LocalCopyOptionsBuilder::new()
            .owner(true)
            .group(true)
            .perms(true)
            .times(true)
            .build()
            .expect("valid options");

        assert!(options.preserve_owner());
        assert!(options.preserve_group());
        assert!(options.preserve_permissions());
        assert!(options.preserve_times());
    }
}

mod integrity_options {
    use super::*;

    #[test]
    fn checksum_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .checksum(true)
            .build()
            .expect("valid options");

        assert!(options.checksum_enabled());
    }

    #[test]
    fn size_only_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .size_only(true)
            .build()
            .expect("valid options");

        assert!(options.size_only_enabled());
    }

    #[test]
    fn ignore_times_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .ignore_times(true)
            .build()
            .expect("valid options");

        assert!(options.ignore_times_enabled());
    }

    #[test]
    fn modify_window_sets_value() {
        let window = Duration::from_secs(5);
        let options = LocalCopyOptionsBuilder::new()
            .modify_window(window)
            .build()
            .expect("valid options");

        assert_eq!(options.modify_window(), window);
    }
}

mod staging_options {
    use super::*;

    #[test]
    fn partial_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .partial(true)
            .build()
            .expect("valid options");

        assert!(options.partial_enabled());
    }

    #[test]
    fn partial_dir_enables_partial() {
        let options = LocalCopyOptionsBuilder::new()
            .partial_dir(Some("/tmp/partial"))
            .build()
            .expect("valid options");

        assert!(options.partial_enabled());
    }

    #[test]
    fn delay_updates_enables_partial() {
        let options = LocalCopyOptionsBuilder::new()
            .delay_updates(true)
            .build()
            .expect("valid options");

        assert!(options.delay_updates_enabled());
        assert!(options.partial_enabled());
    }

    #[test]
    fn inplace_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .inplace(true)
            .build()
            .expect("valid options");

        assert!(options.inplace_enabled());
    }

    #[test]
    fn append_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .append(true)
            .build()
            .expect("valid options");

        assert!(options.append_enabled());
    }

    #[test]
    fn append_verify_enables_append() {
        let options = LocalCopyOptionsBuilder::new()
            .append_verify(true)
            .build()
            .expect("valid options");

        assert!(options.append_enabled());
        assert!(options.append_verify_enabled());
    }
}

mod path_options {
    use super::*;

    #[test]
    fn recursive_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .recursive(true)
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
    }

    #[test]
    fn recursive_disables() {
        let options = LocalCopyOptionsBuilder::new()
            .recursive(false)
            .build()
            .expect("valid options");

        assert!(!options.recursive_enabled());
    }

    #[test]
    fn whole_file_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .whole_file(true)
            .build()
            .expect("valid options");

        assert!(options.whole_file_enabled());
    }

    #[test]
    fn copy_links_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .copy_links(true)
            .build()
            .expect("valid options");

        assert!(options.copy_links_enabled());
    }

    #[test]
    fn preserve_symlinks_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .preserve_symlinks(true)
            .build()
            .expect("valid options");

        assert!(options.links_enabled());
    }

    #[test]
    fn links_alias_works() {
        let options = LocalCopyOptionsBuilder::new()
            .links(true)
            .build()
            .expect("valid options");

        assert!(options.links_enabled());
    }

    #[test]
    fn one_file_system_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .one_file_system(true)
            .build()
            .expect("valid options");

        assert!(options.one_file_system_enabled());
    }
}

mod backup_options {
    use super::*;

    #[test]
    fn backup_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .backup(true)
            .build()
            .expect("valid options");

        assert!(options.backup_enabled());
    }

    #[test]
    fn backup_dir_enables_backup() {
        let options = LocalCopyOptionsBuilder::new()
            .backup_dir(Some("/tmp/backup"))
            .build()
            .expect("valid options");

        assert!(options.backup_enabled());
    }

    #[test]
    fn backup_suffix_enables_backup() {
        let options = LocalCopyOptionsBuilder::new()
            .backup_suffix(Some(".bak"))
            .build()
            .expect("valid options");

        assert!(options.backup_enabled());
    }
}

mod validation {
    use super::*;

    #[test]
    fn valid_configuration_passes() {
        let result = LocalCopyOptionsBuilder::new()
            .recursive(true)
            .preserve_times(true)
            .build();

        assert!(result.is_ok());
    }

    #[test]
    fn size_only_and_checksum_conflict() {
        let result = LocalCopyOptionsBuilder::new()
            .size_only(true)
            .checksum(true)
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::ConflictingOptions {
                option1: "size_only",
                option2: "checksum"
            })
        ));
    }

    #[test]
    fn inplace_and_delay_updates_conflict() {
        let result = LocalCopyOptionsBuilder::new()
            .inplace(true)
            .delay_updates(true)
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::ConflictingOptions {
                option1: "inplace",
                option2: "delay_updates"
            })
        ));
    }

    #[test]
    fn min_greater_than_max_file_size_fails() {
        let result = LocalCopyOptionsBuilder::new()
            .min_file_size(Some(1000))
            .max_file_size(Some(500))
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::InvalidCombination { .. })
        ));
    }

    #[test]
    fn copy_links_and_preserve_symlinks_conflict() {
        let result = LocalCopyOptionsBuilder::new()
            .copy_links(true)
            .preserve_symlinks(true)
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::ConflictingOptions {
                option1: "copy_links",
                option2: "preserve_symlinks"
            })
        ));
    }

    #[test]
    fn build_unchecked_skips_validation() {
        let options = LocalCopyOptionsBuilder::new()
            .size_only(true)
            .checksum(true)
            .build_unchecked();

        assert!(options.size_only_enabled());
        assert!(options.checksum_enabled());
    }
}

mod builder_error {
    use super::*;

    #[test]
    fn conflicting_options_display() {
        let err = BuilderError::ConflictingOptions {
            option1: "foo",
            option2: "bar",
        };
        assert_eq!(err.to_string(), "conflicting options: foo and bar");
    }

    #[test]
    fn invalid_combination_display() {
        let err = BuilderError::InvalidCombination {
            message: "test message".to_string(),
        };
        assert_eq!(err.to_string(), "invalid option combination: test message");
    }

    #[test]
    fn missing_required_option_display() {
        let err = BuilderError::MissingRequiredOption { option: "test" };
        assert_eq!(err.to_string(), "missing required option: test");
    }

    #[test]
    fn value_out_of_range_display() {
        let err = BuilderError::ValueOutOfRange {
            option: "test",
            range: "0-100".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "value out of range for test: expected 0-100"
        );
    }

    #[test]
    fn builder_error_implements_error() {
        let err: Box<dyn std::error::Error> = Box::new(BuilderError::ConflictingOptions {
            option1: "a",
            option2: "b",
        });
        assert!(!err.to_string().is_empty());
    }
}

mod chaining {
    use super::*;

    #[test]
    fn multiple_options_can_be_chained() {
        let options = LocalCopyOptionsBuilder::new()
            .recursive(true)
            .preserve_times(true)
            .preserve_permissions(true)
            .delete(true)
            .compress(true)
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.preserve_times());
        assert!(options.preserve_permissions());
        assert!(options.delete_extraneous());
        assert!(options.compress_enabled());
    }

    #[test]
    fn preset_can_be_modified() {
        let options = LocalCopyOptionsBuilder::new()
            .archive()
            .delete(true)
            .compress(true)
            .build()
            .expect("valid options");

        assert!(options.recursive_enabled());
        assert!(options.delete_extraneous());
        assert!(options.compress_enabled());
    }
}

mod link_options {
    use super::*;

    #[test]
    fn hard_links_enables() {
        let options = LocalCopyOptionsBuilder::new()
            .hard_links(true)
            .build()
            .expect("valid options");

        assert!(options.hard_links_enabled());
    }

    #[test]
    fn link_dest_adds_entry() {
        let options = LocalCopyOptionsBuilder::new()
            .link_dest("/backup")
            .build()
            .expect("valid options");

        assert_eq!(options.link_dest_entries().len(), 1);
    }

    #[test]
    fn link_dests_adds_multiple() {
        let options = LocalCopyOptionsBuilder::new()
            .link_dests(["/backup1", "/backup2"])
            .build()
            .expect("valid options");

        assert_eq!(options.link_dest_entries().len(), 2);
    }
}

mod timeout_options {
    use super::*;

    #[test]
    fn timeout_sets_value() {
        let timeout = Duration::from_secs(60);
        let options = LocalCopyOptionsBuilder::new()
            .timeout(Some(timeout))
            .build()
            .expect("valid options");

        assert_eq!(options.timeout(), Some(timeout));
    }

    #[test]
    fn stop_at_sets_value() {
        let deadline = SystemTime::now();
        let options = LocalCopyOptionsBuilder::new()
            .stop_at(Some(deadline))
            .build()
            .expect("valid options");

        assert!(options.stop_at().is_some());
    }
}
