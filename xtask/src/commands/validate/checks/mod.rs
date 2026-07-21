//! Registry of fidelity checks. Each check is an independent [`Check`] strategy.

mod acl_xattr;
mod adhoc_flags;
mod append_inplace;
mod atimes;
mod backup;
mod banner;
mod capability_string;
mod checksum;
mod chmod;
mod chown;
mod compare_dest;
mod compress;
mod crtimes;
mod cvs_exclude;
mod delete;
mod devices;
mod dirs;
mod dry_run;
mod duplicates;
mod executability;
mod files_from;
mod filters;
mod fuzzy;
mod hard_links;
mod itemize;
mod link_dest;
mod metadata;
mod mkpath;
mod modify_window;
mod one_file_system;
mod progress;
mod protected_regular;
mod prune_empty_dirs;
mod relative;
mod remove_source_files;
mod rsync_path;
mod safe_links;
mod sparse;
mod special_bits;
mod stats;
mod symlinks;
mod total_size;
mod transfer_conditions;
mod trimslash;
mod verbosity;
mod whole_file;
mod xattr;

use super::Check;

/// All checks, in report order.
pub fn all() -> Vec<Box<dyn Check>> {
    vec![
        // Ad-hoc arbitrary-flags parity (only runs when --flags is given).
        Box::new(adhoc_flags::AdHocFlags),
        // Metadata and attributes.
        Box::new(metadata::Metadata),
        Box::new(hard_links::HardLinks),
        Box::new(special_bits::SpecialBits),
        Box::new(chmod::Chmod),
        Box::new(executability::Executability),
        Box::new(chown::Chown),
        Box::new(acl_xattr::AclXattr),
        Box::new(xattr::Xattr),
        Box::new(crtimes::Crtimes),
        Box::new(atimes::Atimes),
        Box::new(symlinks::Symlinks),
        Box::new(devices::Devices),
        // Path selection and layout.
        Box::new(relative::Relative),
        Box::new(trimslash::TrimSlash),
        Box::new(dirs::Dirs),
        Box::new(duplicates::Duplicates),
        Box::new(mkpath::Mkpath),
        Box::new(files_from::FilesFrom),
        Box::new(filters::Filters),
        Box::new(cvs_exclude::CvsExclude),
        Box::new(prune_empty_dirs::PruneEmptyDirs),
        Box::new(one_file_system::OneFileSystem),
        Box::new(sparse::Sparse),
        // Output fidelity.
        Box::new(progress::Progress),
        Box::new(verbosity::Verbosity),
        Box::new(itemize::Itemize),
        Box::new(dry_run::DryRun),
        Box::new(banner::Banner),
        Box::new(stats::Stats),
        Box::new(total_size::TotalSize),
        // Transfer decisions and deletion.
        Box::new(transfer_conditions::TransferConditions),
        Box::new(modify_window::ModifyWindow),
        Box::new(checksum::Checksum),
        Box::new(whole_file::WholeFile),
        Box::new(append_inplace::AppendInplace),
        Box::new(compress::Compress),
        Box::new(delete::Delete),
        Box::new(remove_source_files::RemoveSourceFiles),
        // Alternate destinations.
        Box::new(backup::Backup),
        Box::new(link_dest::LinkDest),
        Box::new(compare_dest::CompareDest),
        // Transport plumbing.
        Box::new(rsync_path::RsyncPath),
        // Security: path-escape and receiver-overwrite safety.
        Box::new(safe_links::SafeLinks),
        Box::new(protected_regular::ProtectedRegular),
        // Wire: server-args capability string.
        Box::new(capability_string::CapabilityString),
    ]
}
