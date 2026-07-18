//! Registry of fidelity checks. Each check is an independent [`Check`] strategy.

mod acl_xattr;
mod banner;
mod itemize;
mod metadata;
mod progress;
mod total_size;

use super::Check;

/// All checks, in report order.
pub fn all() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(metadata::Metadata),
        Box::new(acl_xattr::AclXattr),
        Box::new(progress::Progress),
        Box::new(itemize::Itemize),
        Box::new(banner::Banner),
        Box::new(total_size::TotalSize),
    ]
}
