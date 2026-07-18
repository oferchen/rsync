//! Registry of fidelity checks. Each check is an independent [`Check`] strategy.

mod acl_xattr;
mod backup;
mod banner;
mod checksum;
mod chmod;
mod chown;
mod compress;
mod crtimes;
mod delete;
mod dry_run;
mod filters;
mod hard_links;
mod itemize;
mod link_dest;
mod metadata;
mod progress;
mod prune_empty_dirs;
mod relative;
mod remove_source_files;
mod rsync_path;
mod sparse;
mod special_bits;
mod stats;
mod total_size;
mod verbosity;
mod xattr;

use super::Check;

/// All checks, in report order.
pub fn all() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(metadata::Metadata),
        Box::new(hard_links::HardLinks),
        Box::new(special_bits::SpecialBits),
        Box::new(chmod::Chmod),
        Box::new(chown::Chown),
        Box::new(acl_xattr::AclXattr),
        Box::new(xattr::Xattr),
        Box::new(crtimes::Crtimes),
        Box::new(relative::Relative),
        Box::new(sparse::Sparse),
        Box::new(progress::Progress),
        Box::new(verbosity::Verbosity),
        Box::new(itemize::Itemize),
        Box::new(dry_run::DryRun),
        Box::new(banner::Banner),
        Box::new(filters::Filters),
        Box::new(prune_empty_dirs::PruneEmptyDirs),
        Box::new(delete::Delete),
        Box::new(remove_source_files::RemoveSourceFiles),
        Box::new(backup::Backup),
        Box::new(link_dest::LinkDest),
        Box::new(compress::Compress),
        Box::new(checksum::Checksum),
        Box::new(stats::Stats),
        Box::new(total_size::TotalSize),
        Box::new(rsync_path::RsyncPath),
    ]
}
