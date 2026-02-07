//! Comprehensive unit tests for CLI argument parsing.
//!
//! These tests verify that the argument parser correctly handles:
//! - Short options (-a, -v, -r, -z, etc.)
//! - Combined short options (-avz)
//! - Long options (--archive, --verbose)
//! - Option values (--bwlimit=100K, --port=873)
//! - Invalid arguments and error handling
//! - Path operand parsing (source, destination)

use std::ffi::OsString;

use super::*;
use crate::frontend::arguments::bandwidth::BandwidthArgument;
use crate::frontend::arguments::program_name::ProgramName;
use core::client::{AddressMode, DeleteMode, HumanReadableMode};

/// Helper to parse arguments with "rsync" as the program name.
fn parse_test_args<I, S>(args: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let iter = std::iter::once("rsync".to_string())
        .chain(args.into_iter().map(|s| s.as_ref().to_string()));
    parse_args(iter)
}

// ============================================================================
// Short Option Tests (-a, -v, -r, -z, etc.)
// ============================================================================

mod short_options {
    use super::*;

    #[test]
    fn archive_short_flag() {
        let parsed = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert!(parsed.recursive); // -a implies recursion
    }

    #[test]
    fn verbose_short_flag() {
        let parsed = parse_test_args(["-v", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 1);
    }

    #[test]
    fn recursive_short_flag() {
        let parsed = parse_test_args(["-r", "src/", "dst/"]).expect("parse");
        assert!(parsed.recursive);
    }

    #[test]
    fn compress_short_flag() {
        let parsed = parse_test_args(["-z", "src/", "dst/"]).expect("parse");
        assert!(parsed.compress);
    }

    #[test]
    fn dry_run_short_flag() {
        let parsed = parse_test_args(["-n", "src/", "dst/"]).expect("parse");
        assert!(parsed.dry_run);
    }

    #[test]
    fn links_short_flag() {
        let parsed = parse_test_args(["-l", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.links, Some(true));
    }

    #[test]
    fn perms_short_flag() {
        let parsed = parse_test_args(["-p", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, Some(true));
    }

    #[test]
    fn times_short_flag() {
        let parsed = parse_test_args(["-t", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.times, Some(true));
    }

    #[test]
    fn owner_short_flag() {
        let parsed = parse_test_args(["-o", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.owner, Some(true));
    }

    #[test]
    fn group_short_flag() {
        let parsed = parse_test_args(["-g", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.group, Some(true));
    }

    #[test]
    fn devices_short_flag() {
        let parsed = parse_test_args(["-D", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(true));
        assert_eq!(parsed.specials, Some(true));
    }

    #[test]
    fn hard_links_short_flag() {
        let parsed = parse_test_args(["-H", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.hard_links, Some(true));
    }

    #[test]
    fn sparse_short_flag() {
        let parsed = parse_test_args(["-S", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.sparse, Some(true));
    }

    #[test]
    fn checksum_short_flag() {
        let parsed = parse_test_args(["-c", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum, Some(true));
    }

    #[test]
    fn update_short_flag() {
        let parsed = parse_test_args(["-u", "src/", "dst/"]).expect("parse");
        assert!(parsed.update);
    }

    #[test]
    fn dirs_short_flag() {
        let parsed = parse_test_args(["-d", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.dirs, Some(true));
    }

    #[test]
    fn copy_links_short_flag() {
        let parsed = parse_test_args(["-L", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_links, Some(true));
    }

    #[test]
    fn copy_dirlinks_short_flag() {
        let parsed = parse_test_args(["-k", "src/", "dst/"]).expect("parse");
        assert!(parsed.copy_dirlinks);
    }

    #[test]
    fn keep_dirlinks_short_flag() {
        let parsed = parse_test_args(["-K", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.keep_dirlinks, Some(true));
    }

    #[test]
    fn whole_file_short_flag() {
        let parsed = parse_test_args(["-W", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.whole_file, Some(true));
    }

    #[test]
    fn one_file_system_short_flag() {
        let parsed = parse_test_args(["-x", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.one_file_system, Some(1));
    }

    #[test]
    fn backup_short_flag() {
        let parsed = parse_test_args(["-b", "src/", "dst/"]).expect("parse");
        assert!(parsed.backup);
    }

    #[test]
    fn relative_short_flag() {
        let parsed = parse_test_args(["-R", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.relative, Some(true));
    }

    #[test]
    fn itemize_changes_short_flag() {
        let parsed = parse_test_args(["-i", "src/", "dst/"]).expect("parse");
        assert!(parsed.itemize_changes);
    }

    // Note: -4 and -6 short options are not supported in this implementation
    // Use --ipv4 and --ipv6 long options instead (tested in long_options module)

    #[test]
    fn eight_bit_output_short_flag() {
        let parsed = parse_test_args(["-8", "src/", "dst/"]).expect("parse");
        assert!(parsed.eight_bit_output);
    }

    // Note: -0 (from0) short option is not supported in this implementation
    // Use --from0 long option instead (tested in long_options module)

    // Note: -I (ignore-times) short option is not supported in this implementation
    // Use --ignore-times long option instead (tested in long_options module)

    #[test]
    fn cvs_exclude_short_flag() {
        let parsed = parse_test_args(["-C", "src/", "dst/"]).expect("parse");
        assert!(parsed.cvs_exclude);
    }

    #[test]
    fn fuzzy_short_flag() {
        let parsed = parse_test_args(["-y", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fuzzy, Some(true));
    }

    #[test]
    fn prune_empty_dirs_short_flag() {
        let parsed = parse_test_args(["-m", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.prune_empty_dirs, Some(true));
    }

    #[test]
    fn omit_dir_times_short_flag() {
        let parsed = parse_test_args(["-O", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.omit_dir_times, Some(true));
    }

    #[test]
    fn omit_link_times_short_flag() {
        let parsed = parse_test_args(["-J", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.omit_link_times, Some(true));
    }

    #[test]
    fn atimes_short_flag() {
        let parsed = parse_test_args(["-U", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.atimes, Some(true));
    }

    #[test]
    fn crtimes_short_flag() {
        let parsed = parse_test_args(["-N", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.crtimes, Some(true));
    }

    #[test]
    fn acls_short_flag() {
        let parsed = parse_test_args(["-A", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.acls, Some(true));
    }

    #[test]
    fn xattrs_short_flag() {
        let parsed = parse_test_args(["-X", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.xattrs, Some(true));
    }

    #[test]
    fn executability_short_flag() {
        let parsed = parse_test_args(["-E", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.executability, Some(true));
    }
}

// ============================================================================
// Combined Short Options Tests (-avz)
// ============================================================================

mod combined_short_options {
    use super::*;

    #[test]
    fn avz_combined() {
        let parsed = parse_test_args(["-avz", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.compress);
    }

    #[test]
    fn rvz_combined() {
        let parsed = parse_test_args(["-rvz", "src/", "dst/"]).expect("parse");
        assert!(parsed.recursive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.compress);
    }

    #[test]
    fn vvv_triple_verbose() {
        let parsed = parse_test_args(["-vvv", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 3);
    }

    #[test]
    fn vvvv_quadruple_verbose() {
        let parsed = parse_test_args(["-vvvv", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 4);
    }

    #[test]
    fn avrz_combined() {
        let parsed = parse_test_args(["-avrz", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.recursive);
        assert!(parsed.compress);
    }

    #[test]
    fn rlptgo_d_combined() {
        let parsed = parse_test_args(["-rlptgoD", "src/", "dst/"]).expect("parse");
        assert!(parsed.recursive);
        assert_eq!(parsed.links, Some(true));
        assert_eq!(parsed.perms, Some(true));
        assert_eq!(parsed.times, Some(true));
        assert_eq!(parsed.group, Some(true));
        assert_eq!(parsed.owner, Some(true));
        assert_eq!(parsed.devices, Some(true));
        assert_eq!(parsed.specials, Some(true));
    }

    #[test]
    fn avz_h_combined() {
        let parsed = parse_test_args(["-avzH", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.compress);
        assert_eq!(parsed.hard_links, Some(true));
    }

    #[test]
    fn av_ps_combined() {
        let parsed = parse_test_args(["-avPS", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.partial); // -P implies --partial
        assert_eq!(parsed.sparse, Some(true));
    }

    #[test]
    fn auv_combined() {
        let parsed = parse_test_args(["-auv", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert!(parsed.update);
        assert_eq!(parsed.verbosity, 1);
    }

    #[test]
    fn anv_combined_dry_run() {
        let parsed = parse_test_args(["-anv", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert!(parsed.dry_run);
        assert_eq!(parsed.verbosity, 1);
    }

    #[test]
    fn cvs_exclude_combined() {
        let parsed = parse_test_args(["-avC", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert!(parsed.cvs_exclude);
    }

    #[test]
    fn relative_combined() {
        let parsed = parse_test_args(["-avR", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert_eq!(parsed.relative, Some(true));
    }

    #[test]
    fn checksum_combined() {
        let parsed = parse_test_args(["-avc", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert_eq!(parsed.checksum, Some(true));
    }

    #[test]
    fn whole_file_combined() {
        let parsed = parse_test_args(["-avW", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert_eq!(parsed.whole_file, Some(true));
    }
}

// ============================================================================
// Long Option Tests (--archive, --verbose, etc.)
// ============================================================================

mod long_options {
    use super::*;

    #[test]
    fn archive_long_flag() {
        let parsed = parse_test_args(["--archive", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
    }

    #[test]
    fn verbose_long_flag() {
        let parsed = parse_test_args(["--verbose", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 1);
    }

    #[test]
    fn recursive_long_flag() {
        let parsed = parse_test_args(["--recursive", "src/", "dst/"]).expect("parse");
        assert!(parsed.recursive);
    }

    #[test]
    fn compress_long_flag() {
        let parsed = parse_test_args(["--compress", "src/", "dst/"]).expect("parse");
        assert!(parsed.compress);
    }

    #[test]
    fn dry_run_long_flag() {
        let parsed = parse_test_args(["--dry-run", "src/", "dst/"]).expect("parse");
        assert!(parsed.dry_run);
    }

    #[test]
    fn links_long_flag() {
        let parsed = parse_test_args(["--links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.links, Some(true));
    }

    #[test]
    fn perms_long_flag() {
        let parsed = parse_test_args(["--perms", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, Some(true));
    }

    #[test]
    fn times_long_flag() {
        let parsed = parse_test_args(["--times", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.times, Some(true));
    }

    #[test]
    fn owner_long_flag() {
        let parsed = parse_test_args(["--owner", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.owner, Some(true));
    }

    #[test]
    fn group_long_flag() {
        let parsed = parse_test_args(["--group", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.group, Some(true));
    }

    #[test]
    fn devices_long_flag() {
        let parsed = parse_test_args(["--devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(true));
    }

    #[test]
    fn specials_long_flag() {
        let parsed = parse_test_args(["--specials", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.specials, Some(true));
    }

    #[test]
    fn hard_links_long_flag() {
        let parsed = parse_test_args(["--hard-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.hard_links, Some(true));
    }

    #[test]
    fn sparse_long_flag() {
        let parsed = parse_test_args(["--sparse", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.sparse, Some(true));
    }

    #[test]
    fn checksum_long_flag() {
        let parsed = parse_test_args(["--checksum", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum, Some(true));
    }

    #[test]
    fn update_long_flag() {
        let parsed = parse_test_args(["--update", "src/", "dst/"]).expect("parse");
        assert!(parsed.update);
    }

    #[test]
    fn inplace_long_flag() {
        let parsed = parse_test_args(["--inplace", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.inplace, Some(true));
    }

    #[test]
    fn append_long_flag() {
        let parsed = parse_test_args(["--append", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.append, Some(true));
    }

    #[test]
    fn append_verify_long_flag() {
        let parsed = parse_test_args(["--append-verify", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.append, Some(true));
        assert!(parsed.append_verify);
    }

    #[test]
    fn dirs_long_flag() {
        let parsed = parse_test_args(["--dirs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.dirs, Some(true));
    }

    #[test]
    fn copy_links_long_flag() {
        let parsed = parse_test_args(["--copy-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_links, Some(true));
    }

    #[test]
    fn copy_dirlinks_long_flag() {
        let parsed = parse_test_args(["--copy-dirlinks", "src/", "dst/"]).expect("parse");
        assert!(parsed.copy_dirlinks);
    }

    #[test]
    fn keep_dirlinks_long_flag() {
        let parsed = parse_test_args(["--keep-dirlinks", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.keep_dirlinks, Some(true));
    }

    #[test]
    fn safe_links_long_flag() {
        let parsed = parse_test_args(["--safe-links", "src/", "dst/"]).expect("parse");
        assert!(parsed.safe_links);
    }

    #[test]
    fn munge_links_long_flag() {
        let parsed = parse_test_args(["--munge-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.munge_links, Some(true));
    }

    #[test]
    fn whole_file_long_flag() {
        let parsed = parse_test_args(["--whole-file", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.whole_file, Some(true));
    }

    #[test]
    fn one_file_system_long_flag() {
        let parsed = parse_test_args(["--one-file-system", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.one_file_system, Some(1));
    }

    #[test]
    fn backup_long_flag() {
        let parsed = parse_test_args(["--backup", "src/", "dst/"]).expect("parse");
        assert!(parsed.backup);
    }

    #[test]
    fn relative_long_flag() {
        let parsed = parse_test_args(["--relative", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.relative, Some(true));
    }

    #[test]
    fn itemize_changes_long_flag() {
        let parsed = parse_test_args(["--itemize-changes", "src/", "dst/"]).expect("parse");
        assert!(parsed.itemize_changes);
    }

    #[test]
    fn partial_long_flag() {
        let parsed = parse_test_args(["--partial", "src/", "dst/"]).expect("parse");
        assert!(parsed.partial);
    }

    #[test]
    fn preallocate_long_flag() {
        let parsed = parse_test_args(["--preallocate", "src/", "dst/"]).expect("parse");
        assert!(parsed.preallocate);
    }

    #[test]
    fn delay_updates_long_flag() {
        let parsed = parse_test_args(["--delay-updates", "src/", "dst/"]).expect("parse");
        assert!(parsed.delay_updates);
    }

    #[test]
    fn progress_long_flag() {
        let parsed = parse_test_args(["--progress", "src/", "dst/"]).expect("parse");
        assert!(matches!(
            parsed.progress,
            crate::frontend::progress::ProgressSetting::PerFile
        ));
    }

    #[test]
    fn stats_long_flag() {
        let parsed = parse_test_args(["--stats", "src/", "dst/"]).expect("parse");
        assert!(parsed.stats);
    }

    #[test]
    fn numeric_ids_long_flag() {
        let parsed = parse_test_args(["--numeric-ids", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.numeric_ids, Some(true));
    }

    #[test]
    fn ignore_existing_long_flag() {
        let parsed = parse_test_args(["--ignore-existing", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_existing);
    }

    #[test]
    fn existing_long_flag() {
        let parsed = parse_test_args(["--existing", "src/", "dst/"]).expect("parse");
        assert!(parsed.existing);
    }

    #[test]
    fn ignore_times_long_flag() {
        let parsed = parse_test_args(["--ignore-times", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_times);
    }

    #[test]
    fn ignore_times_short_flag() {
        let parsed = parse_test_args(["-I", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_times);
    }

    #[test]
    fn ignore_times_in_combined_short_flags() {
        let parsed = parse_test_args(["-avI", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_times);
        assert!(parsed.archive);
        assert!(parsed.verbosity > 0);
    }

    #[test]
    fn size_only_long_flag() {
        let parsed = parse_test_args(["--size-only", "src/", "dst/"]).expect("parse");
        assert!(parsed.size_only);
    }

    #[test]
    fn trust_sender_long_flag() {
        let parsed = parse_test_args(["--trust-sender", "src/", "dst/"]).expect("parse");
        assert!(parsed.trust_sender);
    }

    #[test]
    fn list_only_long_flag() {
        let parsed = parse_test_args(["--list-only", "src/", "dst/"]).expect("parse");
        assert!(parsed.list_only);
        assert!(parsed.dry_run); // --list-only implies --dry-run
    }

    #[test]
    fn remove_source_files_long_flag() {
        let parsed = parse_test_args(["--remove-source-files", "src/", "dst/"]).expect("parse");
        assert!(parsed.remove_source_files);
    }

    #[test]
    fn remove_sent_files_alias() {
        let parsed = parse_test_args(["--remove-sent-files", "src/", "dst/"]).expect("parse");
        assert!(parsed.remove_source_files);
    }

    #[test]
    fn force_long_flag() {
        let parsed = parse_test_args(["--force", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.force, Some(true));
    }

    #[test]
    fn implied_dirs_long_flag() {
        let parsed = parse_test_args(["--implied-dirs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.implied_dirs, Some(true));
    }

    #[test]
    fn mkpath_long_flag() {
        let parsed = parse_test_args(["--mkpath", "src/", "dst/"]).expect("parse");
        assert!(parsed.mkpath);
    }

    #[test]
    fn prune_empty_dirs_long_flag() {
        let parsed = parse_test_args(["--prune-empty-dirs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.prune_empty_dirs, Some(true));
    }

    #[test]
    fn cvs_exclude_long_flag() {
        let parsed = parse_test_args(["--cvs-exclude", "src/", "dst/"]).expect("parse");
        assert!(parsed.cvs_exclude);
    }

    #[test]
    fn protect_args_long_flag() {
        let parsed = parse_test_args(["--protect-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.protect_args, Some(true));
    }

    #[test]
    fn no_motd_long_flag() {
        let parsed = parse_test_args(["--no-motd", "src/", "dst/"]).expect("parse");
        assert!(parsed.no_motd);
    }

    #[test]
    fn ipv4_long_flag() {
        let parsed = parse_test_args(["--ipv4", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.address_mode, AddressMode::Ipv4);
    }

    #[test]
    fn ipv6_long_flag() {
        let parsed = parse_test_args(["--ipv6", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.address_mode, AddressMode::Ipv6);
    }

    #[test]
    fn fsync_long_flag() {
        let parsed = parse_test_args(["--fsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fsync, Some(true));
    }

    #[test]
    fn qsort_long_flag() {
        let parsed = parse_test_args(["--qsort", "src/", "dst/"]).expect("parse");
        assert!(parsed.qsort);
    }

    #[test]
    fn omit_link_times_long_flag() {
        let parsed = parse_test_args(["--omit-link-times", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.omit_link_times, Some(true));
    }

    #[test]
    fn from0_long_flag() {
        let parsed = parse_test_args(["--from0", "src/", "dst/"]).expect("parse");
        assert!(parsed.from0);
    }
}

// ============================================================================
// Negation Flag Tests (--no-*)
// ============================================================================

mod negation_flags {
    use super::*;

    #[test]
    fn no_recursive_flag() {
        let parsed = parse_test_args(["--no-recursive", "src/", "dst/"]).expect("parse");
        assert!(!parsed.recursive);
        assert_eq!(parsed.recursive_override, Some(false));
    }

    #[test]
    fn no_perms_flag() {
        let parsed = parse_test_args(["--no-perms", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, Some(false));
    }

    #[test]
    fn no_times_flag() {
        let parsed = parse_test_args(["--no-times", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.times, Some(false));
    }

    #[test]
    fn no_owner_flag() {
        let parsed = parse_test_args(["--no-owner", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.owner, Some(false));
    }

    #[test]
    fn no_group_flag() {
        let parsed = parse_test_args(["--no-group", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.group, Some(false));
    }

    #[test]
    fn no_links_flag() {
        let parsed = parse_test_args(["--no-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.links, Some(false));
    }

    #[test]
    fn no_devices_flag() {
        let parsed = parse_test_args(["--no-devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(false));
    }

    #[test]
    fn no_specials_flag() {
        let parsed = parse_test_args(["--no-specials", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.specials, Some(false));
    }

    #[test]
    fn no_hard_links_flag() {
        let parsed = parse_test_args(["--no-hard-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.hard_links, Some(false));
    }

    #[test]
    fn no_sparse_flag() {
        let parsed = parse_test_args(["--no-sparse", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.sparse, Some(false));
    }

    #[test]
    fn no_checksum_flag() {
        let parsed = parse_test_args(["--no-checksum", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum, Some(false));
    }

    #[test]
    fn no_compress_flag() {
        let parsed = parse_test_args(["--no-compress", "src/", "dst/"]).expect("parse");
        assert!(!parsed.compress);
        assert!(parsed.no_compress);
    }

    #[test]
    fn no_verbose_flag() {
        let parsed = parse_test_args(["-v", "--no-verbose", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 0);
    }

    #[test]
    fn no_progress_flag() {
        let parsed = parse_test_args(["--no-progress", "src/", "dst/"]).expect("parse");
        assert!(matches!(
            parsed.progress,
            crate::frontend::progress::ProgressSetting::Disabled
        ));
    }

    #[test]
    fn no_partial_flag() {
        let parsed = parse_test_args(["--partial", "--no-partial", "src/", "dst/"]).expect("parse");
        assert!(!parsed.partial);
    }

    #[test]
    fn no_inplace_flag() {
        let parsed = parse_test_args(["--no-inplace", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.inplace, Some(false));
    }

    #[test]
    fn no_whole_file_flag() {
        let parsed = parse_test_args(["--no-whole-file", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.whole_file, Some(false));
    }

    #[test]
    fn no_one_file_system_flag() {
        let parsed = parse_test_args(["--no-one-file-system", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.one_file_system, Some(0));
    }

    #[test]
    fn no_relative_flag() {
        let parsed = parse_test_args(["--no-relative", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.relative, Some(false));
    }

    #[test]
    fn no_implied_dirs_flag() {
        let parsed = parse_test_args(["--no-implied-dirs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.implied_dirs, Some(false));
    }

    #[test]
    fn no_mkpath_flag() {
        let parsed = parse_test_args(["--no-mkpath", "src/", "dst/"]).expect("parse");
        assert!(!parsed.mkpath);
    }

    #[test]
    fn no_protect_args_flag() {
        let parsed = parse_test_args(["--no-protect-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.protect_args, Some(false));
    }

    #[test]
    fn no_fsync_flag() {
        let parsed = parse_test_args(["--no-fsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fsync, Some(false));
    }

    #[test]
    fn no_force_flag() {
        let parsed = parse_test_args(["--no-force", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.force, Some(false));
    }

    #[test]
    fn no_fuzzy_flag() {
        let parsed = parse_test_args(["--no-fuzzy", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fuzzy, Some(false));
    }

    #[test]
    fn no_itemize_changes_flag() {
        let parsed = parse_test_args(["--no-itemize-changes", "src/", "dst/"]).expect("parse");
        assert!(!parsed.itemize_changes);
    }
}

// ============================================================================
// Option Value Tests (--bwlimit=100K, --port=873, etc.)
// ============================================================================

mod option_values {
    use super::*;

    #[test]
    fn bwlimit_with_equals() {
        let parsed = parse_test_args(["--bwlimit=100K", "src/", "dst/"]).expect("parse");
        assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Limit(_))));
    }

    #[test]
    fn bwlimit_with_space() {
        let parsed = parse_test_args(["--bwlimit", "100K", "src/", "dst/"]).expect("parse");
        assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Limit(_))));
    }

    #[test]
    fn no_bwlimit_flag() {
        let parsed = parse_test_args(["--no-bwlimit", "src/", "dst/"]).expect("parse");
        assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
    }

    #[test]
    fn port_with_equals() {
        let parsed = parse_test_args(["--port=873", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.daemon_port, Some(873));
    }

    #[test]
    fn port_with_space() {
        let parsed = parse_test_args(["--port", "8873", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.daemon_port, Some(8873));
    }

    #[test]
    fn port_max_value() {
        let parsed = parse_test_args(["--port=65535", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.daemon_port, Some(65535));
    }

    #[test]
    fn port_min_value() {
        let parsed = parse_test_args(["--port=1", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.daemon_port, Some(1));
    }

    #[test]
    fn rsh_with_equals() {
        let parsed = parse_test_args(["--rsh=ssh -p 22", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.remote_shell, Some(OsString::from("ssh -p 22")));
    }

    #[test]
    fn rsh_short_with_space() {
        let parsed = parse_test_args(["-e", "ssh -p 22", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.remote_shell, Some(OsString::from("ssh -p 22")));
    }

    #[test]
    fn rsync_path_with_equals() {
        let parsed =
            parse_test_args(["--rsync-path=/usr/local/bin/rsync", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.rsync_path,
            Some(OsString::from("/usr/local/bin/rsync"))
        );
    }

    #[test]
    fn backup_dir_with_equals() {
        let parsed = parse_test_args(["--backup-dir=/tmp/backup", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.backup_dir, Some(OsString::from("/tmp/backup")));
        assert!(parsed.backup); // backup_dir implies backup
    }

    #[test]
    fn suffix_with_equals() {
        let parsed = parse_test_args(["--suffix=.bak", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.backup_suffix, Some(OsString::from(".bak")));
        assert!(parsed.backup); // suffix implies backup
    }

    #[test]
    fn exclude_with_equals() {
        let parsed = parse_test_args(["--exclude=*.tmp", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.excludes, vec![OsString::from("*.tmp")]);
    }

    #[test]
    fn multiple_excludes() {
        let parsed = parse_test_args([
            "--exclude=*.tmp",
            "--exclude=*.log",
            "--exclude=.git",
            "src/",
            "dst/",
        ])
        .expect("parse");
        assert_eq!(
            parsed.excludes,
            vec![
                OsString::from("*.tmp"),
                OsString::from("*.log"),
                OsString::from(".git")
            ]
        );
    }

    #[test]
    fn include_with_equals() {
        let parsed = parse_test_args(["--include=*.rs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.includes, vec![OsString::from("*.rs")]);
    }

    #[test]
    fn multiple_includes() {
        let parsed =
            parse_test_args(["--include=*.rs", "--include=*.toml", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.includes,
            vec![OsString::from("*.rs"), OsString::from("*.toml")]
        );
    }

    #[test]
    fn filter_with_equals() {
        let parsed = parse_test_args(["--filter=- *.tmp", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.filters, vec![OsString::from("- *.tmp")]);
    }

    #[test]
    fn exclude_from_with_equals() {
        let parsed = parse_test_args(["--exclude-from=/path/to/exclude.txt", "src/", "dst/"])
            .expect("parse");
        assert_eq!(
            parsed.exclude_from,
            vec![OsString::from("/path/to/exclude.txt")]
        );
    }

    #[test]
    fn include_from_with_equals() {
        let parsed = parse_test_args(["--include-from=/path/to/include.txt", "src/", "dst/"])
            .expect("parse");
        assert_eq!(
            parsed.include_from,
            vec![OsString::from("/path/to/include.txt")]
        );
    }

    #[test]
    fn files_from_with_equals() {
        let parsed =
            parse_test_args(["--files-from=/path/to/files.txt", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.files_from,
            vec![OsString::from("/path/to/files.txt")]
        );
    }

    #[test]
    fn temp_dir_with_equals() {
        let parsed =
            parse_test_args(["--temp-dir=/tmp/rsync-temp", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.temp_dir,
            Some(std::path::PathBuf::from("/tmp/rsync-temp"))
        );
    }

    #[test]
    fn partial_dir_with_equals() {
        let parsed =
            parse_test_args(["--partial-dir=/tmp/partial", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.partial_dir,
            Some(std::path::PathBuf::from("/tmp/partial"))
        );
        assert!(parsed.partial); // partial_dir implies partial
    }

    #[test]
    fn compress_level_with_equals() {
        let parsed = parse_test_args(["--compress-level=9", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.compress_level, Some(OsString::from("9")));
    }

    #[test]
    fn max_size_with_equals() {
        let parsed = parse_test_args(["--max-size=100M", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_size, Some(OsString::from("100M")));
    }

    #[test]
    fn min_size_with_equals() {
        let parsed = parse_test_args(["--min-size=1K", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.min_size, Some(OsString::from("1K")));
    }

    #[test]
    fn max_delete_with_equals() {
        let parsed = parse_test_args(["-r", "--max-delete=100", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_delete, Some(OsString::from("100")));
        assert!(parsed.delete_mode.is_enabled()); // max-delete implies delete
    }

    #[test]
    fn block_size_with_equals() {
        let parsed = parse_test_args(["--block-size=8192", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.block_size, Some(OsString::from("8192")));
    }

    #[test]
    fn modify_window_with_equals() {
        let parsed = parse_test_args(["--modify-window=2", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.modify_window, Some(OsString::from("2")));
    }

    #[test]
    fn timeout_with_equals() {
        let parsed = parse_test_args(["--timeout=30", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.timeout, Some(OsString::from("30")));
    }

    #[test]
    fn contimeout_with_equals() {
        let parsed = parse_test_args(["--contimeout=10", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.contimeout, Some(OsString::from("10")));
    }

    #[test]
    fn chown_with_equals() {
        let parsed = parse_test_args(["--chown=user:group", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.chown, Some(OsString::from("user:group")));
    }

    #[test]
    fn chmod_with_equals() {
        let parsed = parse_test_args(["--chmod=u+w,go-rwx", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.chmod, vec![OsString::from("u+w,go-rwx")]);
    }

    #[test]
    fn multiple_chmod() {
        let parsed =
            parse_test_args(["--chmod=u+w", "--chmod=go-rwx", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.chmod,
            vec![OsString::from("u+w"), OsString::from("go-rwx")]
        );
    }

    #[test]
    fn link_dest_with_equals() {
        let parsed = parse_test_args(["--link-dest=/backup/prev", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.link_destinations,
            vec![OsString::from("/backup/prev")]
        );
    }

    #[test]
    fn multiple_link_dest() {
        let parsed = parse_test_args([
            "--link-dest=/backup/prev1",
            "--link-dest=/backup/prev2",
            "src/",
            "dst/",
        ])
        .expect("parse");
        assert_eq!(
            parsed.link_destinations,
            vec![
                OsString::from("/backup/prev1"),
                OsString::from("/backup/prev2")
            ]
        );
    }

    #[test]
    fn compare_dest_with_equals() {
        let parsed =
            parse_test_args(["--compare-dest=/backup/prev", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.compare_destinations,
            vec![OsString::from("/backup/prev")]
        );
    }

    #[test]
    fn copy_dest_with_equals() {
        let parsed = parse_test_args(["--copy-dest=/backup/prev", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.copy_destinations,
            vec![OsString::from("/backup/prev")]
        );
    }

    #[test]
    fn human_readable_with_level() {
        let parsed = parse_test_args(["--human-readable=2", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
    }

    #[test]
    fn log_file_with_equals() {
        let parsed =
            parse_test_args(["--log-file=/var/log/rsync.log", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.log_file, Some(OsString::from("/var/log/rsync.log")));
    }

    #[test]
    fn log_file_format_with_equals() {
        let parsed = parse_test_args(["--log-file-format=%t %n", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.log_file_format, Some(OsString::from("%t %n")));
    }

    #[test]
    fn out_format_with_equals() {
        let parsed = parse_test_args(["--out-format=%n%L", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.out_format, Some(OsString::from("%n%L")));
    }

    #[test]
    fn iconv_with_equals() {
        let parsed = parse_test_args(["--iconv=utf-8,latin1", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.iconv, Some(OsString::from("utf-8,latin1")));
    }

    #[test]
    fn address_with_equals() {
        let parsed = parse_test_args(["--address=192.168.1.1", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.bind_address, Some(OsString::from("192.168.1.1")));
    }

    #[test]
    fn sockopts_with_equals() {
        let parsed =
            parse_test_args(["--sockopts=SO_SNDBUF=65536", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.sockopts, Some(OsString::from("SO_SNDBUF=65536")));
    }

    #[test]
    fn remote_option_with_equals() {
        let parsed =
            parse_test_args(["--remote-option=--bwlimit=100", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.remote_options, vec![OsString::from("--bwlimit=100")]);
    }

    #[test]
    fn multiple_remote_options() {
        let parsed = parse_test_args(["-M", "--bwlimit=100", "-M", "--compress", "src/", "dst/"])
            .expect("parse");
        assert_eq!(
            parsed.remote_options,
            vec![
                OsString::from("--bwlimit=100"),
                OsString::from("--compress")
            ]
        );
    }

    #[test]
    fn info_option() {
        let parsed = parse_test_args(["--info=progress2", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.info, vec![OsString::from("progress2")]);
    }

    #[test]
    fn debug_option() {
        let parsed = parse_test_args(["--debug=all", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.debug, vec![OsString::from("all")]);
    }

    #[test]
    fn protocol_with_equals() {
        let parsed = parse_test_args(["--protocol=31", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.protocol, Some(OsString::from("31")));
    }

    #[test]
    fn password_file_with_equals() {
        let parsed =
            parse_test_args(["--password-file=/path/to/pass.txt", "src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.password_file,
            Some(OsString::from("/path/to/pass.txt"))
        );
    }

    #[test]
    fn outbuf_with_equals() {
        let parsed = parse_test_args(["--outbuf=line", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.outbuf, Some(OsString::from("line")));
    }

    #[test]
    fn max_alloc_with_equals() {
        let parsed = parse_test_args(["--max-alloc=1G", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("1G")));
    }

    #[test]
    fn max_alloc_with_space_separator() {
        let parsed = parse_test_args(["--max-alloc", "256M", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("256M")));
    }

    #[test]
    fn max_alloc_with_bytes_value() {
        let parsed = parse_test_args(["--max-alloc=1048576", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("1048576")));
    }

    #[test]
    fn max_alloc_with_kilobytes() {
        let parsed = parse_test_args(["--max-alloc=512K", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("512K")));
    }

    #[test]
    fn max_alloc_with_megabytes() {
        let parsed = parse_test_args(["--max-alloc=128M", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("128M")));
    }

    #[test]
    fn max_alloc_with_terabytes() {
        let parsed = parse_test_args(["--max-alloc=1T", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.max_alloc, Some(OsString::from("1T")));
    }

    #[test]
    fn max_alloc_default_is_none() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
        assert!(parsed.max_alloc.is_none());
    }

    #[test]
    fn write_batch_with_equals() {
        let parsed = parse_test_args(["--write-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.write_batch, Some(OsString::from("/tmp/batch")));
    }

    #[test]
    fn read_batch_with_equals() {
        let parsed = parse_test_args(["--read-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.read_batch, Some(OsString::from("/tmp/batch")));
    }

    #[test]
    fn only_write_batch_with_equals() {
        let parsed =
            parse_test_args(["--only-write-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.only_write_batch, Some(OsString::from("/tmp/batch")));
    }
}

// ============================================================================
// Delete Mode Tests
// ============================================================================

mod delete_modes {
    use super::*;

    #[test]
    fn delete_with_recursive() {
        let parsed = parse_test_args(["-r", "--delete", "src/", "dst/"]).expect("parse");
        assert!(parsed.delete_mode.is_enabled());
        assert_eq!(parsed.delete_mode, DeleteMode::During);
    }

    #[test]
    fn delete_with_dirs() {
        let parsed = parse_test_args(["-d", "--delete", "src/", "dst/"]).expect("parse");
        assert!(parsed.delete_mode.is_enabled());
    }

    #[test]
    fn delete_before_with_recursive() {
        let parsed = parse_test_args(["-r", "--delete-before", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.delete_mode, DeleteMode::Before);
    }

    #[test]
    fn delete_during_with_recursive() {
        let parsed = parse_test_args(["-r", "--delete-during", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.delete_mode, DeleteMode::During);
    }

    #[test]
    fn delete_delay_with_recursive() {
        let parsed = parse_test_args(["-r", "--delete-delay", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.delete_mode, DeleteMode::Delay);
    }

    #[test]
    fn delete_after_with_recursive() {
        let parsed = parse_test_args(["-r", "--delete-after", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.delete_mode, DeleteMode::After);
    }

    #[test]
    fn delete_excluded_activates_delete() {
        let parsed = parse_test_args(["-r", "--delete-excluded", "src/", "dst/"]).expect("parse");
        assert!(parsed.delete_mode.is_enabled());
        assert!(parsed.delete_excluded);
    }

    #[test]
    fn delete_without_recursive_fails() {
        let result = parse_test_args(["--delete", "src/", "dst/"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("--recursive"));
    }

    #[test]
    fn delete_modes_mutually_exclusive() {
        let result = parse_test_args(["-r", "--delete-before", "--delete-after", "src/", "dst/"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn three_delete_modes_mutually_exclusive() {
        let result = parse_test_args([
            "-r",
            "--delete-before",
            "--delete-during",
            "--delete-after",
            "src/",
            "dst/",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn ignore_errors_flag() {
        let parsed = parse_test_args(["--ignore-errors", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.ignore_errors, Some(true));
    }

    #[test]
    fn no_ignore_errors_flag() {
        let parsed = parse_test_args(["--no-ignore-errors", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.ignore_errors, Some(false));
    }

    #[test]
    fn ignore_missing_args_flag() {
        let parsed = parse_test_args(["--ignore-missing-args", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_missing_args);
    }

    #[test]
    fn delete_missing_args_implies_ignore_missing_args() {
        let parsed = parse_test_args(["--delete-missing-args", "src/", "dst/"]).expect("parse");
        assert!(parsed.ignore_missing_args);
        assert!(parsed.delete_missing_args);
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn invalid_checksum_choice_fails() {
        let result = parse_test_args(["--checksum-choice=invalid", "src/", "dst/"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn usermap_twice_fails() {
        let result = parse_test_args(["--usermap=0:1000", "--usermap=0:1001", "src/", "dst/"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("usermap"));
    }

    #[test]
    fn groupmap_twice_fails() {
        let result = parse_test_args(["--groupmap=0:1000", "--groupmap=0:1001", "src/", "dst/"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("groupmap"));
    }

    #[test]
    fn invalid_checksum_seed_fails() {
        let result = parse_test_args(["--checksum-seed=notanumber", "src/", "dst/"]);
        assert!(result.is_err());
    }
}

// ============================================================================
// Path Operand Tests (source, destination)
// ============================================================================

mod path_operands {
    use super::*;

    #[test]
    fn single_source_single_dest() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![OsString::from("src/"), OsString::from("dst/")]
        );
    }

    #[test]
    fn multiple_sources_single_dest() {
        let parsed = parse_test_args(["src1/", "src2/", "dst/"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![
                OsString::from("src1/"),
                OsString::from("src2/"),
                OsString::from("dst/")
            ]
        );
    }

    #[test]
    fn paths_with_spaces() {
        let parsed = parse_test_args(["path with spaces/", "dest with spaces/"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![
                OsString::from("path with spaces/"),
                OsString::from("dest with spaces/")
            ]
        );
    }

    #[test]
    fn absolute_paths() {
        let parsed = parse_test_args(["/absolute/source", "/absolute/dest"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![
                OsString::from("/absolute/source"),
                OsString::from("/absolute/dest")
            ]
        );
    }

    #[test]
    fn paths_starting_with_dash_after_double_dash() {
        let parsed = parse_test_args(["--", "-source", "-dest"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![OsString::from("-source"), OsString::from("-dest")]
        );
    }

    #[test]
    fn remote_style_paths() {
        let parsed = parse_test_args(["user@host:/remote/path", "local/"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![
                OsString::from("user@host:/remote/path"),
                OsString::from("local/")
            ]
        );
    }

    #[test]
    fn rsync_protocol_style_paths() {
        let parsed = parse_test_args(["rsync://host/module/path", "local/"]).expect("parse");
        assert_eq!(
            parsed.remainder,
            vec![
                OsString::from("rsync://host/module/path"),
                OsString::from("local/")
            ]
        );
    }

    #[test]
    fn paths_with_options_before_operands() {
        // Options must come before operands
        let parsed = parse_test_args(["-a", "-v", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert_eq!(parsed.verbosity, 1);
        assert_eq!(
            parsed.remainder,
            vec![OsString::from("src/"), OsString::from("dst/")]
        );
    }

    #[test]
    fn no_operands_with_help() {
        let parsed = parse_test_args(["--help"]).expect("parse");
        assert!(parsed.show_help);
        assert!(parsed.remainder.is_empty());
    }

    #[test]
    fn no_operands_with_version() {
        let parsed = parse_test_args(["--version"]).expect("parse");
        assert!(parsed.show_version);
        assert!(parsed.remainder.is_empty());
    }
}

// ============================================================================
// Tri-State Flag Behavior Tests
// ============================================================================

mod tri_state_flags {
    use super::*;

    #[test]
    fn positive_then_negative_uses_last() {
        let parsed = parse_test_args(["--perms", "--no-perms", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, Some(false));
    }

    #[test]
    fn negative_then_positive_uses_last() {
        let parsed = parse_test_args(["--no-perms", "--perms", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, Some(true));
    }

    #[test]
    fn multiple_alternations_uses_last() {
        let parsed = parse_test_args([
            "--perms",
            "--no-perms",
            "--perms",
            "--no-perms",
            "--perms",
            "src/",
            "dst/",
        ])
        .expect("parse");
        assert_eq!(parsed.perms, Some(true));
    }

    #[test]
    fn neither_flag_is_none() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
        assert_eq!(parsed.perms, None);
    }

    #[test]
    fn archive_no_recursive_override() {
        let parsed = parse_test_args(["-a", "--no-recursive", "src/", "dst/"]).expect("parse");
        assert!(parsed.archive);
        assert!(!parsed.recursive); // --no-recursive overrides -a's implicit recursive
        assert_eq!(parsed.recursive_override, Some(false));
    }

    #[test]
    fn recursive_negative_first_behavior() {
        let parsed =
            parse_test_args(["--recursive", "--no-recursive", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.recursive_override, Some(false));
    }

    #[test]
    fn dirs_negative_first_behavior() {
        let parsed = parse_test_args(["--dirs", "--no-dirs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.dirs, Some(false));
    }

    #[test]
    fn inc_recursive_positive_first_behavior() {
        let parsed = parse_test_args(["--no-inc-recursive", "--inc-recursive", "src/", "dst/"])
            .expect("parse");
        assert_eq!(parsed.inc_recursive, Some(true));
    }
}

// ============================================================================
// Special Mode Tests
// ============================================================================

mod special_modes {
    use super::*;

    #[test]
    fn server_mode_flag() {
        let parsed = parse_test_args(["--server", "."]).expect("parse");
        assert!(parsed.server_mode);
    }

    #[test]
    fn sender_mode_flag() {
        let parsed = parse_test_args(["--sender", "src/", "dst/"]).expect("parse");
        assert!(parsed.sender_mode);
    }

    #[test]
    fn daemon_mode_flag() {
        let parsed = parse_test_args(["--daemon"]).expect("parse");
        assert!(parsed.daemon_mode);
    }

    #[test]
    fn config_with_daemon() {
        let parsed = parse_test_args(["--daemon", "--config=/etc/rsyncd.conf"]).expect("parse");
        assert!(parsed.daemon_mode);
        assert_eq!(parsed.config, Some(OsString::from("/etc/rsyncd.conf")));
    }

    #[test]
    fn detach_flag() {
        let parsed = parse_test_args(["--detach", "--daemon"]).expect("parse");
        assert_eq!(parsed.detach, Some(true));
    }

    #[test]
    fn no_detach_flag() {
        let parsed = parse_test_args(["--no-detach", "--daemon"]).expect("parse");
        assert_eq!(parsed.detach, Some(false));
    }
}

// ============================================================================
// Help and Version Tests
// ============================================================================

mod help_and_version {
    use super::*;

    #[test]
    fn help_flag() {
        let parsed = parse_test_args(["--help"]).expect("parse");
        assert!(parsed.show_help);
    }

    #[test]
    fn version_flag() {
        let parsed = parse_test_args(["--version"]).expect("parse");
        assert!(parsed.show_version);
    }

    #[test]
    fn short_version_flag() {
        let parsed = parse_test_args(["-V"]).expect("parse");
        assert!(parsed.show_version);
    }
}

// ============================================================================
// Program Name Detection Tests
// ============================================================================

mod program_name_tests {
    use super::*;

    #[test]
    fn rsync_program_name() {
        let parsed = parse_args(["rsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.program_name, ProgramName::Rsync);
    }

    #[test]
    fn oc_rsync_program_name() {
        let parsed = parse_args(["oc-rsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.program_name, ProgramName::OcRsync);
    }

    #[test]
    fn path_based_program_name_rsync() {
        let parsed = parse_args(["/usr/bin/rsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.program_name, ProgramName::Rsync);
    }

    #[test]
    fn path_based_program_name_oc_rsync() {
        let parsed = parse_args(["/usr/local/bin/oc-rsync", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.program_name, ProgramName::OcRsync);
    }
}

// ============================================================================
// Compression Option Tests
// ============================================================================

mod compression_options {
    use super::*;

    #[test]
    fn compress_flag_enables_compression() {
        let parsed = parse_test_args(["-z", "src/", "dst/"]).expect("parse");
        assert!(parsed.compress);
    }

    #[test]
    fn no_compress_flag_disables_compression() {
        let parsed = parse_test_args(["--no-compress", "src/", "dst/"]).expect("parse");
        assert!(!parsed.compress);
        assert!(parsed.no_compress);
    }

    #[test]
    fn compress_then_no_compress() {
        let parsed = parse_test_args(["-z", "--no-compress", "src/", "dst/"]).expect("parse");
        assert!(!parsed.compress);
    }

    #[test]
    fn old_compress_flag() {
        let parsed = parse_test_args(["--old-compress", "src/", "dst/"]).expect("parse");
        assert!(parsed.old_compress);
    }

    #[test]
    fn new_compress_flag() {
        let parsed = parse_test_args(["--new-compress", "src/", "dst/"]).expect("parse");
        assert!(parsed.new_compress);
    }

    #[test]
    fn compress_choice_option() {
        let parsed = parse_test_args(["--compress-choice=zstd", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.compress_choice, Some(OsString::from("zstd")));
    }

    #[test]
    fn skip_compress_option() {
        let parsed =
            parse_test_args(["--skip-compress=gz/jpg/mp3", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.skip_compress, Some(OsString::from("gz/jpg/mp3")));
    }

    #[test]
    fn compress_level_zero_disables() {
        let parsed = parse_test_args(["--compress-level=0", "src/", "dst/"]).expect("parse");
        assert!(!parsed.compress); // Level 0 should disable compression
    }
}

// ============================================================================
// Verbosity Tests
// ============================================================================

mod verbosity_tests {
    use super::*;

    #[test]
    fn single_v_sets_verbosity_1() {
        let parsed = parse_test_args(["-v", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 1);
    }

    #[test]
    fn double_v_sets_verbosity_2() {
        let parsed = parse_test_args(["-vv", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 2);
    }

    #[test]
    fn separate_v_flags_accumulate() {
        let parsed = parse_test_args(["-v", "-v", "-v", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 3);
    }

    #[test]
    fn long_verbose_accumulates() {
        let parsed = parse_test_args(["--verbose", "--verbose", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 2);
    }

    #[test]
    fn quiet_resets_verbosity() {
        let parsed = parse_test_args(["-vvv", "-q", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 0);
    }

    #[test]
    fn no_verbose_resets_verbosity() {
        let parsed = parse_test_args(["-vvv", "--no-verbose", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.verbosity, 0);
    }
}

// ============================================================================
// Blocking IO Tests
// ============================================================================

mod blocking_io_tests {
    use super::*;

    #[test]
    fn blocking_io_flag() {
        let parsed = parse_test_args(["--blocking-io", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.blocking_io, Some(true));
    }

    #[test]
    fn no_blocking_io_flag() {
        let parsed = parse_test_args(["--no-blocking-io", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.blocking_io, Some(false));
    }
}

// ============================================================================
// Old Args Compatibility Tests
// ============================================================================

mod old_args_tests {
    use super::*;

    #[test]
    fn old_args_flag() {
        let parsed = parse_test_args(["--old-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.old_args, Some(true));
    }

    #[test]
    fn no_old_args_flag() {
        let parsed = parse_test_args(["--no-old-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.old_args, Some(false));
    }
}

// ============================================================================
// Default Values Tests
// ============================================================================

mod default_values {
    use super::*;

    #[test]
    fn minimal_args_have_sensible_defaults() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");

        // Boolean defaults
        assert!(!parsed.archive);
        assert!(!parsed.recursive);
        assert!(!parsed.dry_run);
        assert!(!parsed.list_only);
        assert!(!parsed.compress);
        assert!(!parsed.backup);
        assert!(!parsed.stats);
        assert!(!parsed.partial);
        assert!(!parsed.preallocate);
        assert!(!parsed.delay_updates);
        assert!(!parsed.itemize_changes);
        assert!(!parsed.trust_sender);
        assert!(!parsed.server_mode);
        assert!(!parsed.sender_mode);
        assert!(!parsed.daemon_mode);
        assert!(!parsed.cvs_exclude);
        assert!(!parsed.from0);
        assert!(!parsed.no_motd);
        assert!(!parsed.qsort);
        assert!(!parsed.mkpath);
        assert!(!parsed.size_only);
        assert!(!parsed.ignore_times);
        assert!(!parsed.ignore_existing);
        assert!(!parsed.existing);
        assert!(!parsed.ignore_missing_args);
        assert!(!parsed.update);
        assert!(!parsed.copy_dirlinks);
        assert!(!parsed.safe_links);
        assert!(!parsed.copy_devices);
        assert!(!parsed.remove_source_files);
        assert!(!parsed.eight_bit_output);

        // Tri-state defaults (None means unspecified)
        assert_eq!(parsed.perms, None);
        assert_eq!(parsed.times, None);
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.group, None);
        assert_eq!(parsed.links, None);
        assert_eq!(parsed.hard_links, None);
        assert_eq!(parsed.sparse, None);
        assert_eq!(parsed.checksum, None);
        assert_eq!(parsed.whole_file, None);
        assert_eq!(parsed.inplace, None);
        assert_eq!(parsed.append, None);
        assert_eq!(parsed.relative, None);
        assert_eq!(parsed.one_file_system, None);
        assert_eq!(parsed.implied_dirs, None);
        assert_eq!(parsed.force, None);
        assert_eq!(parsed.fuzzy, None);
        assert_eq!(parsed.devices, None);
        assert_eq!(parsed.specials, None);
        assert_eq!(parsed.acls, None);
        assert_eq!(parsed.xattrs, None);
        assert_eq!(parsed.numeric_ids, None);
        assert_eq!(parsed.copy_links, None);
        assert_eq!(parsed.keep_dirlinks, None);
        assert_eq!(parsed.munge_links, None);
        assert_eq!(parsed.fsync, None);
        assert_eq!(parsed.prune_empty_dirs, None);
        assert_eq!(parsed.omit_dir_times, None);
        assert_eq!(parsed.omit_link_times, None);
        assert_eq!(parsed.atimes, None);
        assert_eq!(parsed.crtimes, None);
        assert_eq!(parsed.executability, None);

        // Optional values default to None
        assert_eq!(parsed.bwlimit, None);
        assert_eq!(parsed.daemon_port, None);
        assert_eq!(parsed.remote_shell, None);
        assert_eq!(parsed.rsync_path, None);
        assert_eq!(parsed.backup_dir, None);
        assert_eq!(parsed.backup_suffix, None);
        assert_eq!(parsed.compress_level, None);
        assert_eq!(parsed.compress_choice, None);
        assert_eq!(parsed.max_size, None);
        assert_eq!(parsed.min_size, None);
        assert_eq!(parsed.max_delete, None);
        assert_eq!(parsed.block_size, None);
        assert_eq!(parsed.modify_window, None);
        assert_eq!(parsed.timeout, None);
        assert_eq!(parsed.contimeout, None);
        assert_eq!(parsed.partial_dir, None);
        assert_eq!(parsed.temp_dir, None);
        assert_eq!(parsed.log_file, None);
        assert_eq!(parsed.chown, None);
        assert_eq!(parsed.iconv, None);

        // Vec defaults to empty
        assert!(parsed.excludes.is_empty());
        assert!(parsed.includes.is_empty());
        assert!(parsed.filters.is_empty());
        assert!(parsed.exclude_from.is_empty());
        assert!(parsed.include_from.is_empty());
        assert!(parsed.files_from.is_empty());
        assert!(parsed.chmod.is_empty());
        assert!(parsed.link_destinations.is_empty());
        assert!(parsed.compare_destinations.is_empty());
        assert!(parsed.copy_destinations.is_empty());
        assert!(parsed.remote_options.is_empty());
        assert!(parsed.info.is_empty());
        assert!(parsed.debug.is_empty());

        // Verbosity defaults to 0
        assert_eq!(parsed.verbosity, 0);

        // Delete mode defaults to disabled
        assert_eq!(parsed.delete_mode, DeleteMode::Disabled);

        // Address mode defaults to default
        assert_eq!(parsed.address_mode, AddressMode::Default);
    }

    #[test]
    fn no_operands_is_valid() {
        // Just help flag should work
        let parsed = parse_test_args(["--help"]).expect("parse");
        assert!(parsed.show_help);
        assert!(parsed.remainder.is_empty());
    }
}

// ============================================================================
// Checksum Choice Validation Tests
// ============================================================================

mod checksum_choice_tests {
    use super::*;
    use core::client::StrongChecksumAlgorithm;

    #[test]
    fn checksum_choice_md4() {
        let parsed = parse_test_args(["--checksum-choice=md4", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Md4);
        assert_eq!(choice.file(), StrongChecksumAlgorithm::Md4);
    }

    #[test]
    fn checksum_choice_md5() {
        let parsed = parse_test_args(["--checksum-choice=md5", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Md5);
        assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
    }

    #[test]
    fn checksum_choice_xxh64() {
        let parsed = parse_test_args(["--checksum-choice=xxh64", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh64);
    }

    #[test]
    fn checksum_choice_xxh3() {
        let parsed = parse_test_args(["--checksum-choice=xxh3", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
    }

    #[test]
    fn checksum_choice_xxh128() {
        let parsed = parse_test_args(["--checksum-choice=xxh128", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh128);
    }

    #[test]
    fn checksum_choice_auto() {
        let parsed = parse_test_args(["--checksum-choice=auto", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Auto);
    }

    #[test]
    fn checksum_choice_alias_cc() {
        let parsed = parse_test_args(["--cc=md5", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Md5);
    }

    #[test]
    fn checksum_choice_two_algorithms() {
        let parsed =
            parse_test_args(["--checksum-choice=xxh3,md5", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
        assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
    }

    #[test]
    fn checksum_choice_sha1() {
        let parsed = parse_test_args(["--checksum-choice=sha1", "src/", "dst/"]).expect("parse");
        assert!(parsed.checksum_choice.is_some());
        let choice = parsed.checksum_choice.unwrap();
        assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Sha1);
    }

    #[test]
    fn checksum_seed_valid() {
        let parsed = parse_test_args(["--checksum-seed=12345", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum_seed, Some(12345));
    }

    #[test]
    fn checksum_seed_zero() {
        let parsed = parse_test_args(["--checksum-seed=0", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum_seed, Some(0));
    }

    #[test]
    fn checksum_seed_max_u32() {
        let parsed =
            parse_test_args(["--checksum-seed=4294967295", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum_seed, Some(u32::MAX));
    }

    #[test]
    fn checksum_seed_one() {
        let parsed = parse_test_args(["--checksum-seed=1", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum_seed, Some(1));
    }

    #[test]
    fn checksum_seed_with_checksum_flag() {
        // --checksum and --checksum-seed can be combined
        let parsed =
            parse_test_args(["-c", "--checksum-seed=42", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum, Some(true));
        assert_eq!(parsed.checksum_seed, Some(42));
    }

    #[test]
    fn checksum_seed_overflow_rejected() {
        let result = parse_test_args(["--checksum-seed=4294967296", "src/", "dst/"]);
        assert!(result.is_err(), "u32::MAX + 1 should be rejected");
    }

    #[test]
    fn checksum_seed_negative_rejected() {
        let result = parse_test_args(["--checksum-seed=-1", "src/", "dst/"]);
        assert!(result.is_err(), "negative seed should be rejected");
    }

    #[test]
    fn checksum_seed_defaults_to_none() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
        assert_eq!(parsed.checksum_seed, None, "default should be None");
    }
}

// ============================================================================
// Stop After / Stop At Tests
// ============================================================================

mod stop_time_tests {
    use super::*;

    #[test]
    fn stop_after_option() {
        let parsed = parse_test_args(["--stop-after=60", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.stop_after, Some(OsString::from("60")));
    }

    #[test]
    fn stop_at_option() {
        let parsed = parse_test_args(["--stop-at=12:30", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.stop_at, Some(OsString::from("12:30")));
    }

    #[test]
    fn stop_after_alias_time_limit() {
        let parsed = parse_test_args(["--time-limit=30", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.stop_after, Some(OsString::from("30")));
    }
}

// ============================================================================
// Batch File Conflict Tests
// ============================================================================

mod batch_file_tests {
    use super::*;

    #[test]
    fn write_batch_option() {
        let parsed = parse_test_args(["--write-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.write_batch, Some(OsString::from("/tmp/batch")));
    }

    #[test]
    fn read_batch_option() {
        let parsed = parse_test_args(["--read-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.read_batch, Some(OsString::from("/tmp/batch")));
    }

    #[test]
    fn only_write_batch_option() {
        let parsed =
            parse_test_args(["--only-write-batch=/tmp/batch", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.only_write_batch, Some(OsString::from("/tmp/batch")));
    }
}

// ============================================================================
// Open Noatime Tests
// ============================================================================

mod open_noatime_tests {
    use super::*;

    #[test]
    fn open_noatime_flag() {
        let parsed = parse_test_args(["--open-noatime", "src/", "dst/"]).expect("parse");
        assert!(parsed.open_noatime);
    }

    #[test]
    fn no_open_noatime_flag() {
        let parsed = parse_test_args(["--no-open-noatime", "src/", "dst/"]).expect("parse");
        assert!(!parsed.open_noatime);
        assert!(parsed.no_open_noatime);
    }

    #[test]
    fn open_noatime_then_no_open_noatime() {
        let parsed = parse_test_args(["--open-noatime", "--no-open-noatime", "src/", "dst/"])
            .expect("parse");
        assert!(!parsed.open_noatime);
    }
}

// ============================================================================
// Super / Fake Super Tests
// ============================================================================

mod super_mode_tests {
    use super::*;

    #[test]
    fn super_flag() {
        let parsed = parse_test_args(["--super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.super_mode, Some(true));
    }

    #[test]
    fn no_super_flag() {
        let parsed = parse_test_args(["--no-super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.super_mode, Some(false));
    }

    #[test]
    fn fake_super_flag() {
        let parsed = parse_test_args(["--fake-super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fake_super, Some(true));
    }

    #[test]
    fn no_fake_super_flag() {
        let parsed = parse_test_args(["--no-fake-super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fake_super, Some(false));
    }

    #[test]
    fn super_then_no_super() {
        let parsed = parse_test_args(["--super", "--no-super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.super_mode, Some(false));
    }

    #[test]
    fn fake_super_then_no_fake_super() {
        let parsed =
            parse_test_args(["--fake-super", "--no-fake-super", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.fake_super, Some(false));
    }
}

// ============================================================================
// Device Handling Tests
// ============================================================================

mod device_tests {
    use super::*;

    #[test]
    fn copy_devices_flag() {
        let parsed = parse_test_args(["--copy-devices", "src/", "dst/"]).expect("parse");
        assert!(parsed.copy_devices);
    }

    #[test]
    fn write_devices_flag() {
        let parsed = parse_test_args(["--write-devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.write_devices, Some(true));
    }

    #[test]
    fn no_write_devices_flag() {
        let parsed = parse_test_args(["--no-write-devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.write_devices, Some(false));
    }

    #[test]
    fn devices_flag() {
        let parsed = parse_test_args(["--devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(true));
    }

    #[test]
    fn no_devices_flag() {
        let parsed = parse_test_args(["--no-devices", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(false));
    }

    #[test]
    fn specials_flag() {
        let parsed = parse_test_args(["--specials", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.specials, Some(true));
    }

    #[test]
    fn no_specials_flag() {
        let parsed = parse_test_args(["--no-specials", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.specials, Some(false));
    }

    #[test]
    fn archive_devices_short_d() {
        let parsed = parse_test_args(["-D", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.devices, Some(true));
        assert_eq!(parsed.specials, Some(true));
    }
}

// ============================================================================
// Human Readable Tests
// ============================================================================

mod human_readable_tests {
    use super::*;

    #[test]
    fn human_readable_no_value() {
        let parsed = parse_test_args(["-h", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
    }

    #[test]
    fn human_readable_level_0() {
        let parsed = parse_test_args(["--human-readable=0", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
    }

    #[test]
    fn human_readable_level_1() {
        let parsed = parse_test_args(["--human-readable=1", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
    }

    #[test]
    fn human_readable_level_2() {
        let parsed = parse_test_args(["--human-readable=2", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
    }

    #[test]
    fn no_human_readable_flag() {
        let parsed = parse_test_args(["--no-human-readable", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
    }

    #[test]
    fn no_h_alias() {
        let parsed = parse_test_args(["--no-h", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
    }
}

// ============================================================================
// Iconv Tests
// ============================================================================

mod iconv_tests {
    use super::*;

    #[test]
    fn iconv_option() {
        let parsed = parse_test_args(["--iconv=utf-8", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.iconv, Some(OsString::from("utf-8")));
        assert!(!parsed.no_iconv);
    }

    #[test]
    fn no_iconv_flag() {
        let parsed = parse_test_args(["--no-iconv", "src/", "dst/"]).expect("parse");
        assert!(parsed.no_iconv);
        assert_eq!(parsed.iconv, None);
    }
}

// ============================================================================
// Additional Copy Link Tests
// ============================================================================

mod copy_link_tests {
    use super::*;

    #[test]
    fn copy_unsafe_links_flag() {
        let parsed = parse_test_args(["--copy-unsafe-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_unsafe_links, Some(true));
    }

    #[test]
    fn no_copy_unsafe_links_flag() {
        let parsed = parse_test_args(["--no-copy-unsafe-links", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_unsafe_links, Some(false));
    }
}

// ============================================================================
// Msgs to Stderr Tests
// ============================================================================

mod msgs_stderr_tests {
    use super::*;

    #[test]
    fn msgs2stderr_flag() {
        let parsed = parse_test_args(["--msgs2stderr", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.msgs_to_stderr, Some(true));
    }

    #[test]
    fn no_msgs2stderr_flag() {
        let parsed = parse_test_args(["--no-msgs2stderr", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.msgs_to_stderr, Some(false));
    }

    #[test]
    fn stderr_mode_option() {
        let parsed = parse_test_args(["--stderr=errors", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.stderr_mode, Some(OsString::from("errors")));
    }
}

// ============================================================================
// Dparam Tests
// ============================================================================

mod dparam_tests {
    use super::*;

    #[test]
    fn dparam_single() {
        let parsed = parse_test_args(["--dparam=foo=bar", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.dparam, vec![OsString::from("foo=bar")]);
    }

    #[test]
    fn dparam_multiple() {
        let parsed = parse_test_args(["--dparam=foo=bar", "--dparam=baz=qux", "src/", "dst/"])
            .expect("parse");
        assert_eq!(
            parsed.dparam,
            vec![OsString::from("foo=bar"), OsString::from("baz=qux")]
        );
    }
}

// ============================================================================
// Additional Time Tests
// ============================================================================

mod time_option_tests {
    use super::*;

    #[test]
    fn no_omit_dir_times_flag() {
        let parsed = parse_test_args(["--no-omit-dir-times", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.omit_dir_times, Some(false));
    }

    #[test]
    fn no_omit_link_times_flag() {
        let parsed = parse_test_args(["--no-omit-link-times", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.omit_link_times, Some(false));
    }

    #[test]
    fn no_atimes_flag() {
        let parsed = parse_test_args(["--no-atimes", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.atimes, Some(false));
    }

    #[test]
    fn no_crtimes_flag() {
        let parsed = parse_test_args(["--no-crtimes", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.crtimes, Some(false));
    }
}

// ============================================================================
// Delay Updates Tests
// ============================================================================

mod delay_updates_tests {
    use super::*;

    #[test]
    fn delay_updates_flag() {
        let parsed = parse_test_args(["--delay-updates", "src/", "dst/"]).expect("parse");
        assert!(parsed.delay_updates);
    }

    #[test]
    fn no_delay_updates_flag() {
        let parsed = parse_test_args(["--delay-updates", "--no-delay-updates", "src/", "dst/"])
            .expect("parse");
        assert!(!parsed.delay_updates);
    }
}

// ============================================================================
// No Backup Tests
// ============================================================================

mod backup_tests {
    use super::*;

    #[test]
    fn no_backup_flag() {
        let parsed = parse_test_args(["--backup", "--no-backup", "src/", "dst/"]).expect("parse");
        assert!(!parsed.backup);
    }

    #[test]
    fn no_b_alias() {
        let parsed = parse_test_args(["-b", "--no-b", "src/", "dst/"]).expect("parse");
        assert!(!parsed.backup);
    }
}

// ============================================================================
// Partial Progress Tests
// ============================================================================

mod partial_progress_tests {
    use super::*;

    #[test]
    fn p_short_option() {
        let parsed = parse_test_args(["-P", "src/", "dst/"]).expect("parse");
        assert!(parsed.partial);
    }

    #[test]
    fn p_short_option_multiple() {
        let parsed = parse_test_args(["-PP", "src/", "dst/"]).expect("parse");
        assert!(parsed.partial);
    }
}

// ============================================================================
// Additional Alias Tests
// ============================================================================

mod alias_tests {
    use super::*;

    #[test]
    fn existing_alias_ignore_non_existing() {
        let parsed = parse_test_args(["--ignore-non-existing", "src/", "dst/"]).expect("parse");
        assert!(parsed.existing);
    }

    #[test]
    fn del_alias_for_delete_during() {
        let parsed = parse_test_args(["-r", "--del", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.delete_mode, DeleteMode::During);
    }

    #[test]
    fn tmp_dir_alias() {
        let parsed = parse_test_args(["--tmp-dir=/tmp/test", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.temp_dir, Some(std::path::PathBuf::from("/tmp/test")));
    }

    #[test]
    fn log_format_alias() {
        let parsed = parse_test_args(["--log-format=%n", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.out_format, Some(OsString::from("%n")));
    }

    #[test]
    fn compress_level_alias_zl() {
        let parsed = parse_test_args(["--zl=5", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.compress_level, Some(OsString::from("5")));
    }

    #[test]
    fn compress_choice_alias_zc() {
        let parsed = parse_test_args(["--zc=zstd", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.compress_choice, Some(OsString::from("zstd")));
    }

    #[test]
    fn inc_recursive_alias_i_r() {
        let parsed = parse_test_args(["--i-r", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.inc_recursive, Some(true));
    }

    #[test]
    fn no_inc_recursive_alias() {
        let parsed = parse_test_args(["--no-i-r", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.inc_recursive, Some(false));
    }

    #[test]
    fn implied_dirs_alias_i_d() {
        let parsed = parse_test_args(["--i-d", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.implied_dirs, Some(true));
    }

    #[test]
    fn no_implied_dirs_alias() {
        let parsed = parse_test_args(["--no-i-d", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.implied_dirs, Some(false));
    }

    #[test]
    fn old_dirs_alias_for_no_mkpath() {
        let parsed = parse_test_args(["--mkpath", "--old-dirs", "src/", "dst/"]).expect("parse");
        assert!(!parsed.mkpath);
    }

    #[test]
    fn secluded_args_alias() {
        let parsed = parse_test_args(["--secluded-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.protect_args, Some(true));
    }

    #[test]
    fn no_secluded_args_alias() {
        let parsed = parse_test_args(["--no-secluded-args", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.protect_args, Some(false));
    }
}

// ============================================================================
// Numeric IDs Tests
// ============================================================================

mod numeric_ids_tests {
    use super::*;

    #[test]
    fn numeric_ids_flag() {
        let parsed = parse_test_args(["--numeric-ids", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.numeric_ids, Some(true));
    }

    #[test]
    fn no_numeric_ids_flag() {
        let parsed = parse_test_args(["--no-numeric-ids", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.numeric_ids, Some(false));
    }
}

// ============================================================================
// ACLS and Xattrs Negation Tests
// ============================================================================

mod acls_xattrs_tests {
    use super::*;

    #[test]
    fn no_acls_flag() {
        let parsed = parse_test_args(["--no-acls", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.acls, Some(false));
    }

    #[test]
    fn no_xattrs_flag() {
        let parsed = parse_test_args(["--no-xattrs", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.xattrs, Some(false));
    }
}

// ============================================================================
// No Executability Tests
// ============================================================================

mod executability_tests {
    use super::*;

    #[test]
    fn no_executability_flag() {
        let parsed = parse_test_args(["--no-executability", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.executability, Some(false));
    }

    #[test]
    fn executability_then_no_executability() {
        let parsed = parse_test_args(["-E", "--no-executability", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.executability, Some(false));
    }
}

// ============================================================================
// Copy As Tests
// ============================================================================

mod copy_as_tests {
    use super::*;

    #[test]
    fn copy_as_user() {
        let parsed = parse_test_args(["--copy-as=root", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_as, Some(OsString::from("root")));
    }

    #[test]
    fn copy_as_user_group() {
        let parsed = parse_test_args(["--copy-as=root:wheel", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.copy_as, Some(OsString::from("root:wheel")));
    }
}

// ============================================================================
// Usermap and Groupmap Tests
// ============================================================================

mod map_tests {
    use super::*;

    #[test]
    fn usermap_single() {
        let parsed = parse_test_args(["--usermap=0:1000", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.usermap, Some(OsString::from("0:1000")));
    }

    #[test]
    fn groupmap_single() {
        let parsed = parse_test_args(["--groupmap=0:1000", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.groupmap, Some(OsString::from("0:1000")));
    }
}

// ============================================================================
// Motd Tests
// ============================================================================

mod motd_tests {
    use super::*;

    #[test]
    fn motd_flag_overrides_no_motd() {
        let parsed = parse_test_args(["--no-motd", "--motd", "src/", "dst/"]).expect("parse");
        assert!(!parsed.no_motd);
    }

    #[test]
    fn no_motd_then_motd() {
        let parsed = parse_test_args(["--no-motd", "--motd", "src/", "dst/"]).expect("parse");
        assert!(!parsed.no_motd);
    }
}

// ============================================================================
// Early Input Tests
// ============================================================================

mod early_input_tests {
    use super::*;

    #[test]
    fn early_input_option() {
        let parsed = parse_test_args(["--early-input=/tmp/early", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.early_input, Some(OsString::from("/tmp/early")));
    }
}

// ============================================================================
// From0 Tests
// ============================================================================

mod from0_tests {
    use super::*;

    #[test]
    fn from0_flag() {
        let parsed = parse_test_args(["--from0", "src/", "dst/"]).expect("parse");
        assert!(parsed.from0);
    }

    #[test]
    fn no_from0_disables() {
        let parsed = parse_test_args(["--from0", "--no-from0", "src/", "dst/"]).expect("parse");
        assert!(!parsed.from0);
    }
}

// ============================================================================
// No Timeout Tests
// ============================================================================

mod timeout_tests {
    use super::*;

    #[test]
    fn no_timeout_option() {
        let parsed =
            parse_test_args(["--timeout=30", "--no-timeout", "src/", "dst/"]).expect("parse");
        // --no-timeout should override --timeout
        assert_eq!(parsed.timeout, None);
    }

    #[test]
    fn no_contimeout_option() {
        let parsed =
            parse_test_args(["--contimeout=10", "--no-contimeout", "src/", "dst/"]).expect("parse");
        // --no-contimeout should override --contimeout
        assert_eq!(parsed.contimeout, None);
    }
}

// ============================================================================
// Connect Program Tests
// ============================================================================

mod connect_program_tests {
    use super::*;

    #[test]
    fn connect_program_option() {
        let parsed =
            parse_test_args(["--connect-program=nc %H %P", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.connect_program, Some(OsString::from("nc %H %P")));
    }
}

// ============================================================================
// Rsync Filter Shortcut Tests
// ============================================================================

mod rsync_filter_tests {
    use super::*;

    #[test]
    fn rsync_filter_single_f() {
        let parsed = parse_test_args(["-F", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.rsync_filter_shortcuts, 1);
    }

    #[test]
    fn rsync_filter_double_ff() {
        let parsed = parse_test_args(["-FF", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.rsync_filter_shortcuts, 2);
    }
}

// ============================================================================
// Name Level and Itemize Changes Tests
// ============================================================================

mod name_level_tests {
    use super::*;
    use crate::frontend::progress::NameOutputLevel;

    #[test]
    fn itemize_changes_sets_name_level() {
        let parsed = parse_test_args(["-i", "src/", "dst/"]).expect("parse");
        assert!(parsed.itemize_changes);
        assert_eq!(parsed.name_level, NameOutputLevel::UpdatedOnly);
        assert!(parsed.name_overridden);
    }

    #[test]
    fn no_itemize_changes_clears_name_level() {
        let parsed = parse_test_args(["--no-itemize-changes", "src/", "dst/"]).expect("parse");
        assert!(!parsed.itemize_changes);
        assert_eq!(parsed.name_level, NameOutputLevel::Disabled);
        assert!(parsed.name_overridden);
    }

    #[test]
    fn itemize_then_no_itemize() {
        let parsed =
            parse_test_args(["-i", "--no-itemize-changes", "src/", "dst/"]).expect("parse");
        assert!(!parsed.itemize_changes);
    }
}

// ============================================================================
// Progress Setting Tests
// ============================================================================

mod progress_setting_tests {
    use super::*;
    use crate::frontend::progress::ProgressSetting;

    #[test]
    fn progress_flag_sets_per_file() {
        let parsed = parse_test_args(["--progress", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.progress, ProgressSetting::PerFile);
    }

    #[test]
    fn no_progress_sets_disabled() {
        let parsed = parse_test_args(["--no-progress", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.progress, ProgressSetting::Disabled);
    }

    #[test]
    fn default_progress_is_unspecified() {
        let parsed = parse_test_args(["src/", "dst/"]).expect("parse");
        assert_eq!(parsed.progress, ProgressSetting::Unspecified);
    }
}

// ============================================================================
// 8-bit Output Negation Tests
// ============================================================================

mod eight_bit_output_tests {
    use super::*;

    #[test]
    fn no_8_bit_output_flag() {
        let parsed = parse_test_args(["-8", "--no-8-bit-output", "src/", "dst/"]).expect("parse");
        assert!(!parsed.eight_bit_output);
    }

    #[test]
    fn no_8_alias() {
        let parsed = parse_test_args(["-8", "--no-8", "src/", "dst/"]).expect("parse");
        assert!(!parsed.eight_bit_output);
    }
}

// ============================================================================
// Empty Value Tests
// ============================================================================

mod empty_value_tests {
    use super::*;

    #[test]
    fn empty_rsync_path_is_filtered() {
        let parsed = parse_test_args(["--rsync-path=", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.rsync_path, None);
    }

    #[test]
    fn empty_connect_program_is_filtered() {
        let parsed = parse_test_args(["--connect-program=", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed.connect_program, None);
    }
}

// ============================================================================
// Parsed Args Derived Trait Tests
// ============================================================================

mod parsed_args_traits {
    use super::*;

    #[test]
    fn parsed_args_clone() {
        let parsed = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        let cloned = parsed.clone();
        assert_eq!(parsed, cloned);
    }

    #[test]
    fn parsed_args_debug() {
        let parsed = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        let debug = format!("{parsed:?}");
        assert!(debug.contains("archive"));
    }

    #[test]
    fn parsed_args_eq() {
        let parsed1 = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        let parsed2 = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        assert_eq!(parsed1, parsed2);
    }

    #[test]
    fn parsed_args_ne() {
        let parsed1 = parse_test_args(["-a", "src/", "dst/"]).expect("parse");
        let parsed2 = parse_test_args(["-v", "src/", "dst/"]).expect("parse");
        assert_ne!(parsed1, parsed2);
    }
}
