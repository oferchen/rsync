//! Tests for [`ServerConfigBuilder`].

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::builder::ServerConfigBuilder;
use super::error::BuilderError;
use super::{ConnectionConfig, DeletionConfig, FileSelectionConfig, ServerConfig, WriteConfig};
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

mod builder_creation {
    use super::*;

    #[test]
    fn new_creates_builder_with_defaults() {
        let config = ServerConfigBuilder::new().build().expect("valid config");
        assert_eq!(config.role, ServerRole::Receiver);
        assert_eq!(config.protocol, ProtocolVersion::NEWEST);
        assert!(config.flag_string.is_empty());
        assert!(config.args.is_empty());
    }

    #[test]
    fn default_trait_matches_new() {
        let c1 = ServerConfigBuilder::new().build().expect("valid");
        let c2 = ServerConfigBuilder::default().build().expect("valid");
        assert_eq!(c1.role, c2.role);
        assert_eq!(c1.protocol, c2.protocol);
        assert_eq!(c1.flag_string, c2.flag_string);
    }

    #[test]
    fn builder_method_on_server_config() {
        let config = ServerConfig::builder().build().expect("valid config");
        assert_eq!(config.role, ServerRole::Receiver);
    }
}

mod chaining {
    use super::*;

    #[test]
    fn setters_chain_correctly() {
        let mut builder = ServerConfigBuilder::new();
        let config = builder
            .role(ServerRole::Generator)
            .flag_string("-rv")
            .args(vec![OsString::from("/src"), OsString::from("/dst")])
            .trust_sender(true)
            .do_stats(true)
            .qsort(true)
            .build()
            .expect("valid config");

        assert_eq!(config.role, ServerRole::Generator);
        assert_eq!(config.flag_string, "-rv");
        assert_eq!(config.args.len(), 2);
        assert!(config.trust_sender);
        assert!(config.do_stats);
        assert!(config.qsort);
    }

    #[test]
    fn write_setters_chain() {
        let mut builder = ServerConfigBuilder::new();
        let config = builder
            .fsync(true)
            .inplace(true)
            .write_devices(true)
            .build()
            .expect("valid config");

        assert!(config.write.fsync);
        assert!(config.write.inplace);
        assert!(config.write.write_devices);
    }

    #[test]
    fn deletion_setters_chain() {
        let mut builder = ServerConfigBuilder::new();
        let config = builder
            .max_delete(Some(100))
            .ignore_errors(true)
            .late_delete(true)
            .build()
            .expect("valid config");

        assert_eq!(config.deletion.max_delete, Some(100));
        assert!(config.deletion.ignore_errors);
        assert!(config.deletion.late_delete);
    }

    #[test]
    fn file_selection_setters_chain() {
        let mut builder = ServerConfigBuilder::new();
        let config = builder
            .min_file_size(Some(1024))
            .max_file_size(Some(1_000_000))
            .ignore_existing(true)
            .existing_only(false)
            .size_only(true)
            .from0(true)
            .build()
            .expect("valid config");

        assert_eq!(config.file_selection.min_file_size, Some(1024));
        assert_eq!(config.file_selection.max_file_size, Some(1_000_000));
        assert!(config.file_selection.ignore_existing);
        assert!(!config.file_selection.existing_only);
        assert!(config.file_selection.size_only);
        assert!(config.file_selection.from0);
    }

    #[test]
    fn connection_setters_chain() {
        let mut builder = ServerConfigBuilder::new();
        let config = builder
            .client_mode(true)
            .is_daemon_connection(true)
            .build()
            .expect("valid config");

        assert!(config.connection.client_mode);
        assert!(config.connection.is_daemon_connection);
    }

    #[test]
    fn builder_reusable_after_build() {
        let mut builder = ServerConfigBuilder::new();
        builder.role(ServerRole::Generator).flag_string("-r");

        let c1 = builder.build().expect("first build");
        let c2 = builder.build().expect("second build");

        assert_eq!(c1.role, c2.role);
        assert_eq!(c1.flag_string, c2.flag_string);
    }
}

mod defaults {
    use super::*;

    #[test]
    fn default_write_config() {
        let config = ServerConfigBuilder::new().build().expect("valid");
        assert!(!config.write.fsync);
        assert!(!config.write.inplace);
        assert!(!config.write.inplace_partial);
        assert!(!config.write.write_devices);
        assert!(!config.write.delay_updates);
    }

    #[test]
    fn default_deletion_config() {
        let config = ServerConfigBuilder::new().build().expect("valid");
        assert!(config.deletion.max_delete.is_none());
        assert!(!config.deletion.ignore_errors);
        assert!(!config.deletion.late_delete);
    }

    #[test]
    fn default_connection_config() {
        let config = ServerConfigBuilder::new().build().expect("valid");
        assert!(!config.connection.client_mode);
        assert!(!config.connection.is_daemon_connection);
        assert!(config.connection.filter_rules.is_empty());
        assert!(config.connection.compression_level.is_none());
        assert!(config.connection.compress_choice.is_none());
        assert!(config.connection.files_from_data.is_none());
    }

    #[test]
    fn default_file_selection_config() {
        let config = ServerConfigBuilder::new().build().expect("valid");
        assert!(config.file_selection.min_file_size.is_none());
        assert!(config.file_selection.max_file_size.is_none());
        assert!(!config.file_selection.ignore_existing);
        assert!(!config.file_selection.existing_only);
        assert!(!config.file_selection.size_only);
    }

    #[test]
    fn default_optional_fields() {
        let config = ServerConfigBuilder::new().build().expect("valid");
        assert!(config.checksum_seed.is_none());
        assert!(config.checksum_choice.is_none());
        assert!(!config.trust_sender);
        assert!(config.stop_at.is_none());
        assert!(!config.qsort);
        assert!(!config.has_partial_dir);
        assert!(config.backup_dir.is_none());
        assert!(config.backup_suffix.is_none());
        assert!(config.daemon_filter_rules.is_empty());
        assert!(!config.do_stats);
        assert!(config.temp_dir.is_none());
        assert!(config.skip_compress.is_none());
        assert!(!config.fake_super);
    }

    /// Daemon-side `fake super = yes` flows through the builder into
    /// `ServerConfig.fake_super`, which the receiver then forwards into
    /// `MetadataOptions.fake_super` so ownership/special-file metadata is
    /// stored in the `user.rsync.%stat` xattr instead of being applied to
    /// inodes (upstream: `clientserver.c:1106-1107`).
    #[test]
    fn fake_super_round_trips_through_builder() {
        let enabled = ServerConfigBuilder::new()
            .fake_super(true)
            .build()
            .expect("valid");
        assert!(enabled.fake_super);

        let disabled = ServerConfigBuilder::new()
            .fake_super(false)
            .build()
            .expect("valid");
        assert!(!disabled.fake_super);
    }
}

mod validation {
    use super::*;

    #[test]
    fn valid_configuration_passes() {
        let result = ServerConfigBuilder::new()
            .role(ServerRole::Generator)
            .flag_string("-rv")
            .args(vec![OsString::from("/path")])
            .build();

        assert!(result.is_ok());
    }

    #[test]
    fn inplace_and_delay_updates_conflict() {
        let result = ServerConfigBuilder::new()
            .inplace(true)
            .delay_updates(true)
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::ConflictingOptions {
                option1: "--inplace",
                option2: "--delay-updates",
            })
        ));
    }

    #[test]
    fn append_and_partial_dir_conflict() {
        let mut builder = ServerConfigBuilder::new();
        builder.flags(ParsedServerFlags {
            append: true,
            ..ParsedServerFlags::default()
        });
        builder.has_partial_dir(true);

        let result = builder.build();

        assert!(matches!(
            result,
            Err(BuilderError::ConflictingOptions {
                option1: "--append",
                option2: "--partial-dir",
            })
        ));
    }

    #[test]
    fn min_greater_than_max_file_size_fails() {
        let result = ServerConfigBuilder::new()
            .min_file_size(Some(1000))
            .max_file_size(Some(500))
            .build();

        assert!(matches!(
            result,
            Err(BuilderError::InvalidCombination { .. })
        ));
    }

    #[test]
    fn equal_min_max_file_size_passes() {
        let result = ServerConfigBuilder::new()
            .min_file_size(Some(1000))
            .max_file_size(Some(1000))
            .build();

        assert!(result.is_ok());
    }

    #[test]
    fn only_inplace_without_delay_updates_passes() {
        let result = ServerConfigBuilder::new().inplace(true).build();
        assert!(result.is_ok());
    }

    #[test]
    fn only_delay_updates_without_inplace_passes() {
        let result = ServerConfigBuilder::new().delay_updates(true).build();
        assert!(result.is_ok());
    }

    #[test]
    fn build_unchecked_skips_validation() {
        let mut builder = ServerConfigBuilder::new();
        builder.inplace(true).delay_updates(true);

        let config = builder.build_unchecked();
        assert!(config.write.inplace);
        assert!(config.write.delay_updates);
    }
}

mod composite_setters {
    use super::*;

    #[test]
    fn write_config_setter_replaces_entire_config() {
        let write = WriteConfig {
            fsync: true,
            inplace: true,
            ..Default::default()
        };
        let config = ServerConfigBuilder::new()
            .write(write.clone())
            .build()
            .expect("valid");

        assert_eq!(config.write, write);
    }

    #[test]
    fn deletion_config_setter_replaces_entire_config() {
        let deletion = DeletionConfig {
            max_delete: Some(50),
            ignore_errors: true,
            late_delete: true,
        };
        let config = ServerConfigBuilder::new()
            .deletion(deletion.clone())
            .build()
            .expect("valid");

        assert_eq!(config.deletion, deletion);
    }

    #[test]
    fn connection_config_setter_replaces_entire_config() {
        let connection = ConnectionConfig {
            client_mode: true,
            is_daemon_connection: true,
            ..Default::default()
        };
        let config = ServerConfigBuilder::new()
            .connection(connection.clone())
            .build()
            .expect("valid");

        assert_eq!(config.connection, connection);
    }

    #[test]
    fn file_selection_config_setter_replaces_entire_config() {
        let selection = FileSelectionConfig {
            min_file_size: Some(100),
            max_file_size: Some(10_000),
            size_only: true,
            ..Default::default()
        };
        let config = ServerConfigBuilder::new()
            .file_selection(selection.clone())
            .build()
            .expect("valid");

        assert_eq!(config.file_selection, selection);
    }
}

mod builder_error {
    use super::*;

    #[test]
    fn conflicting_options_display() {
        let err = BuilderError::ConflictingOptions {
            option1: "--foo",
            option2: "--bar",
        };
        assert_eq!(err.to_string(), "conflicting options: --foo and --bar");
    }

    #[test]
    fn invalid_combination_display() {
        let err = BuilderError::InvalidCombination {
            message: "test message".to_string(),
        };
        assert_eq!(err.to_string(), "invalid option combination: test message");
    }

    #[test]
    fn builder_error_implements_error_trait() {
        let err: Box<dyn std::error::Error> = Box::new(BuilderError::ConflictingOptions {
            option1: "a",
            option2: "b",
        });
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn builder_error_eq() {
        let err1 = BuilderError::ConflictingOptions {
            option1: "a",
            option2: "b",
        };
        let err2 = BuilderError::ConflictingOptions {
            option1: "a",
            option2: "b",
        };
        assert_eq!(err1, err2);
    }

    #[test]
    fn builder_error_ne() {
        let err1 = BuilderError::ConflictingOptions {
            option1: "a",
            option2: "b",
        };
        let err2 = BuilderError::ConflictingOptions {
            option1: "c",
            option2: "d",
        };
        assert_ne!(err1, err2);
    }
}
