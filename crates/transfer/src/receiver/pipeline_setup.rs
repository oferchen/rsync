//! Receiver pipeline setup state and post-transfer attribute helpers.
//!
//! Carries the checksum, metadata, and ACL state shared across transfer modes,
//! applies cached ACLs to destination files, and compiles daemon-side filter
//! rules.

use std::num::NonZeroU8;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::filters::FilterRuleWireFormat;
use protocol::flist::FileEntry;

use filters::FilterSet;
use metadata::AclIdMapper;

use crate::disk_commit::LiveAclCache;

/// Shared configuration produced by `ReceiverContext::setup_transfer`.
///
/// Groups the checksum, metadata, and ACL state that is common to all
/// transfer modes (sync, pipelined, incremental). Passed to the pipeline
/// loop and the redo pass.
pub(in crate::receiver) struct PipelineSetup {
    pub(in crate::receiver) dest_dir: PathBuf,
    pub(in crate::receiver) metadata_opts: metadata::MetadataOptions,
    pub(in crate::receiver) checksum_length: NonZeroU8,
    pub(in crate::receiver) checksum_algorithm: signature::SignatureAlgorithm,
    /// ACL cache populated during flist reception and extended by every
    /// sub-list. Shared with the disk commit thread so cached ACLs can be
    /// applied after file metadata. `None` when `--acls` is not active.
    pub(in crate::receiver) acl_cache: Option<LiveAclCache>,
    /// Cross-host id remapper for named ACL entries, built from the received
    /// uid/gid id-lists plus `--usermap`/`--groupmap`. Shared with the disk
    /// commit thread via `Arc`. `None` when `--acls` is not active.
    ///
    /// upstream: acls.c:1430-1450 `match_racl_ids()` / `match_acl_ids()` convert
    /// every named ACL entry id through the same table as file owners.
    pub(in crate::receiver) acl_id_map: Option<Arc<AclIdMapper>>,
    /// Parent-dirfd carrier rooted at the destination tree.
    ///
    /// Opened once via [`fast_io::secure_open_dir`] when `setup_transfer`
    /// resolves the destination path. Threaded through the receiver
    /// pipeline (directory, symlink, special and file-commit passes) so
    /// destination writes use `*at` syscalls beneath this fd instead of
    /// re-walking the path through the kernel.
    ///
    /// `None` on Unix when the destination root cannot be opened (for
    /// example because it does not yet exist - the receiver will create
    /// it later and the carrier stays absent for the duration of the
    /// transfer). The field does not exist on Windows, where the carrier is
    /// not used (handle-based NTFS APIs).
    #[cfg(unix)]
    pub(in crate::receiver) sandbox: Option<Arc<fast_io::DirSandbox>>,
}

/// Applies ACLs from the receiver's ACL cache to a destination file.
///
/// Looks up the file entry's `acl_ndx` and optional `def_acl_ndx` in the cache
/// and applies the corresponding ACL to `destination`. No-op when `acl_cache`
/// is `None` or the entry has no ACL index.
///
/// `dest_root`, when supplied, is the destination tree the ACL write is pinned
/// beneath (see `metadata::apply_acls_from_cache`).
///
/// # Upstream Reference
///
/// Mirrors upstream `rsync.c:800-802` `set_file_attrs()`, which applies the
/// ACL via `set_acl_fdat()` after ownership and before the final `chmod`.
pub(in crate::receiver) fn apply_acls_from_receiver_cache(
    destination: &std::path::Path,
    entry: &FileEntry,
    acl_cache: Option<&LiveAclCache>,
    id_map: Option<&AclIdMapper>,
    follow_symlinks: bool,
    dest_root: Option<&std::path::Path>,
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
        &cache.read().expect("ACL cache lock poisoned"),
        access_ndx,
        entry.def_acl_ndx(),
        follow_symlinks,
        Some(entry.mode()),
        id_map,
        dest_root,
    )
}

/// Compiles daemon filter rules from wire format into a `FilterSet`.
///
/// Returns `Some(filter_set)` when rules are present, `None` when empty.
/// Used by the receiver to reject daemon-excluded files before accepting
/// transfer data, mirroring upstream `check_filter(&daemon_filter_list, ...)`
/// in `receiver.c:903-905` `recv_files()`.
///
/// # Upstream Reference
///
/// - `clientserver.c:933-952` `rsync_module()` - daemon filter list is built from
///   module filter/include_from/include/exclude_from/exclude directives
/// - `receiver.c:903-905` - per-file check against `daemon_filter_list`
pub(in crate::receiver) fn compile_daemon_filter_set(
    rules: &[FilterRuleWireFormat],
) -> Option<FilterSet> {
    use protocol::filters::RuleType;

    // A per-directory merge contributes no flat rule here apart from its `e`
    // self-exclude (see `compile_daemon_merge_configs`), and an eager `merge`
    // was expanded when the list was built.
    let flat = rules.iter().any(|rule| match rule.rule_type {
        RuleType::Merge => false,
        RuleType::DirMerge => rule.exclude_from_merge,
        _ => true,
    });
    if !flat {
        return None;
    }
    // The module's rules compile exactly as a received list does, so every
    // flag a rule carries - negate, xattr-only, directory-only - reaches the
    // match.
    crate::receiver::transfer::parse_wire_filters_for_receiver(rules)
        .ok()
        .map(|(set, _merge_configs)| set)
}

/// Compiles the per-directory merge directives out of a module's filter rules.
///
/// The companion to [`compile_daemon_filter_set`]: that function owns the flat
/// rules, this one owns the `dir-merge` directives they cannot express. Both
/// read the same wire list, and
/// [`DaemonFilterGate`](crate::receiver::DaemonFilterGate) evaluates them
/// together.
///
/// # Upstream Reference
///
/// `exclude.c:349-391` `add_rule()` registers every `FILTRULE_PERDIR_MERGE` rule
/// in the global `mergelist_parents`, whichever list it was added to - so a
/// rule from a module's `filter` directive, which lands in
/// `daemon_filter_list` (`clientserver.c:934`), is registered alongside the
/// transfer's own and `push_local_filters` fills it per directory.
pub(in crate::receiver) fn compile_daemon_merge_configs(
    rules: &[FilterRuleWireFormat],
) -> Vec<filters::DirMergeConfig> {
    use protocol::filters::RuleType;

    rules
        .iter()
        .filter(|rule| rule.rule_type == RuleType::DirMerge)
        .map(crate::receiver::transfer::dir_merge_config_from_wire)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{compile_daemon_filter_set, compile_daemon_merge_configs};
    use protocol::filters::{FilterRuleWireFormat, RuleType};
    use std::path::Path;

    fn exclude(pattern: &str) -> FilterRuleWireFormat {
        FilterRuleWireFormat::exclude(pattern)
    }

    /// A module's `filter = : NAME` reaches the receiver as a `DirMerge` wire
    /// rule, and the two compilers split it correctly: the flat set has nothing
    /// to say about it, and the merge compiler names the file to read per
    /// directory.
    ///
    /// ⚠ Both halves are asserted. A compiler that turned the directive into an
    /// exclude of `.rsync-filter` would leave the flat set non-empty, and one
    /// that dropped it entirely would leave the merge list empty - the pair
    /// tells those apart.
    #[test]
    fn a_dir_merge_directive_compiles_to_a_merge_config_and_no_flat_rule() {
        let rules = vec![FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rsync-filter".into(),
            ..FilterRuleWireFormat::default()
        }];

        assert!(
            compile_daemon_filter_set(&rules).is_none(),
            "a per-directory merge is not a flat rule"
        );
        let configs = compile_daemon_merge_configs(&rules);
        assert_eq!(configs.len(), 1, "the directive must survive as a config");
        assert_eq!(configs[0].filename(), ".rsync-filter");
    }

    /// A plain rule contributes nothing to the merge list, so the two
    /// compilers cannot both be reading the whole wire list indiscriminately.
    #[test]
    fn a_plain_rule_contributes_no_merge_config() {
        assert!(compile_daemon_merge_configs(&[exclude("foo")]).is_empty());
    }

    /// An unsided `clear`, as a module's `filter = clear` directive produces.
    fn clear() -> FilterRuleWireFormat {
        FilterRuleWireFormat {
            rule_type: RuleType::Clear,
            ..FilterRuleWireFormat::default()
        }
    }

    /// The discriminating cell.
    ///
    /// Ground truth, MEASURED 2026-11-03 against a real rsync 3.5.1 daemon on
    /// loopback with `read only = no`, pushing `foo` and `keep` into a module
    /// declaring `filter = - foo` then `filter = clear`: BOTH files land.
    /// oc refused `foo` and exited 23, because the clear was dropped here and
    /// the exclude survived.
    #[test]
    fn a_clear_pops_the_exclude_that_precedes_it() {
        let filters = compile_daemon_filter_set(&[exclude("foo"), clear()])
            .expect("two rules compile to a filter set");
        assert!(
            filters.allows(Path::new("foo"), false),
            "the clear must pop the preceding exclude"
        );
    }

    /// Non-vacuity companion for
    /// [`a_clear_pops_the_exclude_that_precedes_it`].
    ///
    /// Without this, that test would also pass if the compiled set simply
    /// allowed everything - which is exactly what a filter set built from zero
    /// surviving rules does. This one proves the exclude bites when no clear
    /// follows it, so the pin above is measuring the clear and not the absence
    /// of a filter.
    #[test]
    fn an_exclude_alone_still_refuses() {
        let filters = compile_daemon_filter_set(&[exclude("foo")])
            .expect("one rule compiles to a filter set");
        assert!(
            !filters.allows(Path::new("foo"), false),
            "an exclude with no clear after it must still refuse"
        );
    }

    /// A clear pops what PRECEDED it and nothing more, so a rule written after
    /// it still applies. Distinguishes "the clear emptied the list" from "the
    /// clear disabled filtering", which the first two tests alone cannot tell
    /// apart.
    ///
    /// Same fixture measured against rsync 3.5.1 with a third directive
    /// `filter = - keep`: `foo` lands, `keep` is refused.
    #[test]
    fn a_rule_after_the_clear_still_applies() {
        let filters = compile_daemon_filter_set(&[exclude("foo"), clear(), exclude("keep")])
            .expect("three rules compile to a filter set");
        assert!(
            filters.allows(Path::new("foo"), false),
            "the clear must pop the exclude written before it"
        );
        assert!(
            !filters.allows(Path::new("keep"), false),
            "a rule written after the clear must survive it"
        );
    }
}
