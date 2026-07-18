//! Receiver pipeline setup state and post-transfer attribute helpers.
//!
//! Carries the checksum, metadata, and ACL state shared across transfer modes,
//! applies cached ACLs to destination files, and compiles daemon-side filter
//! rules.

use std::num::NonZeroU8;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::acl::AclCache;
use protocol::filters::FilterRuleWireFormat;
use protocol::flist::FileEntry;

use filters::FilterSet;
use metadata::AclIdMapper;

/// Shared configuration produced by [`ReceiverContext::setup_transfer`].
///
/// Groups the checksum, metadata, and ACL state that is common to all
/// transfer modes (sync, pipelined, incremental). Passed to the pipeline
/// loop and the redo pass.
pub(in crate::receiver) struct PipelineSetup {
    pub(in crate::receiver) dest_dir: PathBuf,
    pub(in crate::receiver) metadata_opts: metadata::MetadataOptions,
    pub(in crate::receiver) checksum_length: NonZeroU8,
    pub(in crate::receiver) checksum_algorithm: signature::SignatureAlgorithm,
    /// ACL cache populated during flist reception. Shared with the disk commit
    /// thread via `Arc` so cached ACLs can be applied after file metadata.
    /// `None` when `--acls` is not active.
    pub(in crate::receiver) acl_cache: Option<Arc<AclCache>>,
    /// Cross-host id remapper for named ACL entries, built from the received
    /// uid/gid id-lists plus `--usermap`/`--groupmap`. Shared with the disk
    /// commit thread via `Arc`. `None` when `--acls` is not active.
    ///
    /// upstream: acls.c:1059-1081 `match_acl_ids()` converts every named ACL
    /// entry id through the same table as file owners.
    pub(in crate::receiver) acl_id_map: Option<Arc<AclIdMapper>>,
    /// Parent-dirfd carrier rooted at the destination tree.
    ///
    /// Opened once via [`fast_io::secure_open_dir`] when `setup_transfer`
    /// resolves the destination path. Threaded through the receiver
    /// pipeline so the SEC-1.f-j cutover sites can replace path-based
    /// syscalls with their `*at` siblings without re-walking the path
    /// through the kernel. The carrier is threaded through to
    /// [`ReceiverContext`] but no syscalls are migrated yet; the existing
    /// path-based code paths remain the active code (SEC-1.e).
    ///
    /// `None` on Unix when the destination root cannot be opened (for
    /// example because it does not yet exist - the receiver will create
    /// it later and the carrier stays absent for the duration of the
    /// transfer). `None` on Windows where the carrier is not used
    /// (handle-based NTFS APIs, see SEC-1.l audit).
    #[cfg(unix)]
    pub(in crate::receiver) sandbox: Option<Arc<fast_io::DirSandbox>>,
}

/// Applies ACLs from the receiver's ACL cache to a destination file.
///
/// Looks up the file entry's `acl_ndx` and optional `def_acl_ndx` in the cache
/// and applies the corresponding ACL to `destination`. No-op when `acl_cache`
/// is `None` or the entry has no ACL index.
///
/// # Upstream Reference
///
/// Mirrors upstream `set_file_attrs()` in receiver.c which calls `set_acl()`
/// after setting permissions, times, and ownership.
pub(in crate::receiver) fn apply_acls_from_receiver_cache(
    destination: &std::path::Path,
    entry: &FileEntry,
    acl_cache: Option<&AclCache>,
    id_map: Option<&AclIdMapper>,
    follow_symlinks: bool,
) -> Result<(), metadata::MetadataError> {
    let cache = match acl_cache {
        Some(c) => c,
        None => return Ok(()),
    };
    let access_ndx = match entry.acl_ndx() {
        Some(ndx) => ndx,
        None => return Ok(()),
    };
    metadata::apply_acls_from_cache(
        destination,
        cache,
        access_ndx,
        entry.def_acl_ndx(),
        follow_symlinks,
        Some(entry.mode()),
        id_map,
    )
}

/// Compiles daemon filter rules from wire format into a `FilterSet`.
///
/// Returns `Some(filter_set)` when rules are present, `None` when empty.
/// Used by the receiver to reject daemon-excluded files before accepting
/// transfer data, mirroring upstream `check_filter(&daemon_filter_list, ...)`
/// in `receiver.c:599-604`.
///
/// # Upstream Reference
///
/// - `clientserver.c:874-893` - daemon filter list is built from module
///   filter/exclude/include/exclude_from/include_from directives
/// - `receiver.c:599-604` - per-file check against daemon_filter_list
pub(in crate::receiver) fn compile_daemon_filter_set(
    rules: &[FilterRuleWireFormat],
) -> Option<FilterSet> {
    use filters::FilterRule;
    use protocol::filters::RuleType;

    if rules.is_empty() {
        return None;
    }

    let filter_rules: Vec<FilterRule> = rules
        .iter()
        .filter_map(|wire_rule| {
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(&wire_rule.pattern),
                RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
                RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
                RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
                RuleType::Clear | RuleType::DirMerge | RuleType::Merge => return None,
            };

            if wire_rule.sender_side || wire_rule.receiver_side {
                rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
            }
            if wire_rule.perishable {
                rule = rule.with_perishable(true);
            }
            if wire_rule.anchored {
                rule = rule.anchor_to_root();
            }

            Some(rule)
        })
        .collect();

    if filter_rules.is_empty() {
        return None;
    }

    FilterSet::from_rules(filter_rules).ok()
}
