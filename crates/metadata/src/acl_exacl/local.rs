//! One local-copy entry's ACLs, read once by the sender half and applied by
//! the receiver half.

use std::io;
use std::path::Path;

use protocol::acl::{AclCache, RsyncAcl};

use super::apply::{apply_acls_from_cache, store_acls_via_fake_super};
use super::read::read_rsync_acl;
use crate::MetadataError;

/// An entry's access ACL and, for a directory, its default ACL, as the
/// sender half of a local copy reads them.
#[derive(Clone, Debug)]
pub struct LocalAcls {
    access: RsyncAcl,
    default: Option<RsyncAcl>,
    mode: u32,
}

/// A source ACL read that failed.
#[derive(Debug)]
pub struct AclReadError {
    /// Whether the default ACL, rather than the access ACL, failed.
    pub is_default: bool,
    /// The failed read.
    pub error: io::Error,
}

impl AclReadError {
    /// The ACL type upstream names in its `get_acl` message.
    ///
    /// upstream: acls.c:102-116 str_acl_type().
    #[must_use]
    pub const fn acl_type(&self) -> &'static str {
        if self.is_default {
            "ACL_TYPE_DEFAULT"
        } else {
            "ACL_TYPE_ACCESS"
        }
    }
}

impl LocalAcls {
    /// Reads `path`'s ACLs; `mode` fills a filesystem without ACLs.
    ///
    /// Under `--fake-super` an ACL stashed in the source's own `%aacl`/`%dacl`
    /// attribute wins over the filesystem ACL.
    ///
    /// # Errors
    ///
    /// Returns the read upstream fails on, so the caller can drop the entry.
    ///
    /// upstream: acls.c:586-759 get_rsync_acl(), its `am_root < 0` arm first.
    pub fn read(
        path: &Path,
        mode: u32,
        is_dir: bool,
        fake_super: bool,
    ) -> Result<Self, AclReadError> {
        let read = |is_default: bool| {
            if fake_super
                && let Ok(Some(acl)) = crate::fake_super::load_fake_super_acl(path, !is_default)
            {
                return Ok(acl);
            }
            read_rsync_acl(path, mode, is_default)
                .map_err(|error| AclReadError { is_default, error })
        };
        let access = read(false)?;
        let default = if is_dir { Some(read(true)?) } else { None };
        Ok(Self {
            access,
            default,
            mode,
        })
    }

    /// The access ACL.
    #[must_use]
    pub const fn access(&self) -> &RsyncAcl {
        &self.access
    }

    /// The default ACL, read only for a directory.
    #[must_use]
    pub const fn default_acl(&self) -> Option<&RsyncAcl> {
        self.default.as_ref()
    }

    /// Applies the ACLs to `destination` as the receiver half does, or stores
    /// them in its `%aacl`/`%dacl` attributes under `--fake-super`.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError`] when writing the ACL fails.
    ///
    /// upstream: acls.c:set_acl() and its `am_root < 0` arm.
    pub fn apply(
        &self,
        destination: &Path,
        fake_super: bool,
        confine_root: Option<&Path>,
    ) -> Result<(), MetadataError> {
        let mut cache = AclCache::new();
        let access = cache.store_access(self.access.clone());
        let default = self.default.clone().map(|acl| cache.store_default(acl));
        if fake_super {
            store_acls_via_fake_super(destination, &cache, access, default, true, self.mode)
        } else {
            apply_acls_from_cache(
                destination,
                &cache,
                access,
                default,
                true,
                Some(self.mode),
                None,
                confine_root,
            )
        }
    }
}
