//! Generic filter evaluation for file-list entry types.
//!
//! Extends [`FilterSet`] and [`FilterChain`] with methods that accept any
//! `FileEntryAccessor` implementor, eliminating the manual `name()` /
//! `is_dir()` extraction that every consumer currently performs before
//! calling the `&Path`-based filter API.
//!
//! This module is feature-gated behind `flat-flist` and is the filters-side
//! counterpart of RSS-A.7.d: it allows filter evaluation to work with both
//! the legacy `FileEntry` and the arena-backed `FlatFileEntry` through the
//! shared `FileEntryAccessor` trait.
//!
//! # Upstream Reference
//!
//! - `exclude.c:check_filter()` - first-match-wins evaluation loop
//! - `generator.c:1261` - daemon filter check before directory creation
//! - `receiver.c:599` - daemon filter check before file transfer

use std::path::Path;

use protocol::flist::FileEntryAccessor;

use crate::{FilterChain, FilterSet};

impl FilterSet {
    /// Returns `true` if the entry should be included in the transfer.
    ///
    /// Extracts the entry's name and directory flag via `FileEntryAccessor`
    /// and delegates to [`allows`](Self::allows).
    #[must_use]
    pub fn allows_entry<T: FileEntryAccessor>(&self, entry: &T) -> bool {
        self.allows(Path::new(entry.name()), entry.is_dir())
    }

    /// Returns `true` if deleting the entry on the receiver is permitted.
    ///
    /// Extracts the entry's name and directory flag via `FileEntryAccessor`
    /// and delegates to [`allows_deletion`](Self::allows_deletion).
    #[must_use]
    pub fn allows_entry_deletion<T: FileEntryAccessor>(&self, entry: &T) -> bool {
        self.allows_deletion(Path::new(entry.name()), entry.is_dir())
    }

    /// Returns `true` if the entry may be removed during `--delete-excluded`.
    ///
    /// Extracts the entry's name and directory flag via `FileEntryAccessor`
    /// and delegates to
    /// [`allows_deletion_when_excluded_removed`](Self::allows_deletion_when_excluded_removed).
    #[must_use]
    pub fn allows_entry_deletion_when_excluded_removed<T: FileEntryAccessor>(
        &self,
        entry: &T,
    ) -> bool {
        self.allows_deletion_when_excluded_removed(Path::new(entry.name()), entry.is_dir())
    }

    /// Returns `true` when a directory entry is excluded by a non-directory-specific rule.
    ///
    /// Used by `--prune-empty-dirs` to decide whether to still descend into
    /// an excluded directory. Extracts the entry's name via
    /// `FileEntryAccessor` and delegates to
    /// [`excluded_dir_by_non_dir_rule`](Self::excluded_dir_by_non_dir_rule).
    #[must_use]
    pub fn entry_excluded_dir_by_non_dir_rule<T: FileEntryAccessor>(&self, entry: &T) -> bool {
        self.excluded_dir_by_non_dir_rule(Path::new(entry.name()))
    }
}

impl FilterChain {
    /// Returns `true` if the entry should be included in the transfer.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. Extracts the entry's name and directory flag via
    /// `FileEntryAccessor`.
    #[must_use]
    pub fn allows_entry<T: FileEntryAccessor>(&self, entry: &T) -> bool {
        self.allows(Path::new(entry.name()), entry.is_dir())
    }

    /// Returns `true` if deleting the entry on the receiver is permitted.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. Extracts the entry's name and directory flag via
    /// `FileEntryAccessor`.
    #[must_use]
    pub fn allows_entry_deletion<T: FileEntryAccessor>(&self, entry: &T) -> bool {
        self.allows_deletion(Path::new(entry.name()), entry.is_dir())
    }
}

#[cfg(test)]
mod tests {
    use protocol::flist::FileEntry;

    use crate::{FilterChain, FilterRule, FilterSet};

    #[test]
    fn allows_entry_includes_unmatched_file() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
        let entry = FileEntry::new_file("readme.txt".into(), 100, 0o644);
        assert!(set.allows_entry(&entry));
    }

    #[test]
    fn allows_entry_excludes_matched_file() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
        let entry = FileEntry::new_file("backup.bak".into(), 100, 0o644);
        assert!(!set.allows_entry(&entry));
    }

    #[test]
    fn allows_entry_directory_only_rule() {
        let set = FilterSet::from_rules([FilterRule::exclude("cache/")]).unwrap();
        let dir = FileEntry::new_directory("cache".into(), 0o755);
        let file = FileEntry::new_file("cache".into(), 100, 0o644);
        assert!(!set.allows_entry(&dir));
        // Directory-only rule does not match a regular file named "cache".
        assert!(set.allows_entry(&file));
    }

    #[test]
    fn allows_entry_empty_filter_allows_all() {
        let set = FilterSet::default();
        let entry = FileEntry::new_file("anything.txt".into(), 0, 0o644);
        assert!(set.allows_entry(&entry));
    }

    #[test]
    fn allows_entry_deletion_protected() {
        let set = FilterSet::from_rules([FilterRule::protect("important.dat")]).unwrap();
        let entry = FileEntry::new_file("important.dat".into(), 1024, 0o644);
        assert!(!set.allows_entry_deletion(&entry));
    }

    #[test]
    fn allows_entry_deletion_unprotected() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let entry = FileEntry::new_file("data.txt".into(), 512, 0o644);
        assert!(set.allows_entry_deletion(&entry));
    }

    #[test]
    fn allows_entry_deletion_when_excluded_removed_excluded_file() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
        let entry = FileEntry::new_file("debug.log".into(), 256, 0o644);
        assert!(set.allows_entry_deletion_when_excluded_removed(&entry));
    }

    #[test]
    fn allows_entry_deletion_when_excluded_removed_included_file() {
        let set = FilterSet::from_rules([FilterRule::include("keep.txt")]).unwrap();
        let entry = FileEntry::new_file("keep.txt".into(), 128, 0o644);
        assert!(!set.allows_entry_deletion_when_excluded_removed(&entry));
    }

    #[test]
    fn entry_excluded_dir_by_non_dir_rule_generic_pattern() {
        let set = FilterSet::from_rules([FilterRule::exclude("*")]).unwrap();
        let dir = FileEntry::new_directory("cache".into(), 0o755);
        assert!(set.entry_excluded_dir_by_non_dir_rule(&dir));
    }

    #[test]
    fn entry_excluded_dir_by_non_dir_rule_dir_specific() {
        let set = FilterSet::from_rules([FilterRule::exclude("cache/")]).unwrap();
        let dir = FileEntry::new_directory("cache".into(), 0o755);
        assert!(!set.entry_excluded_dir_by_non_dir_rule(&dir));
    }

    #[test]
    fn chain_allows_entry_global_rules() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let chain = FilterChain::new(global);
        let entry = FileEntry::new_file("scratch.tmp".into(), 64, 0o644);
        assert!(!chain.allows_entry(&entry));
    }

    #[test]
    fn chain_allows_entry_no_match_includes() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let chain = FilterChain::new(global);
        let entry = FileEntry::new_file("readme.md".into(), 200, 0o644);
        assert!(chain.allows_entry(&entry));
    }

    #[test]
    fn chain_allows_entry_deletion_protected() {
        let global = FilterSet::from_rules([FilterRule::protect("*.conf")]).unwrap();
        let chain = FilterChain::new(global);
        let entry = FileEntry::new_file("app.conf".into(), 300, 0o644);
        assert!(!chain.allows_entry_deletion(&entry));
    }

    #[test]
    fn chain_allows_entry_deletion_unprotected() {
        let global = FilterSet::default();
        let chain = FilterChain::new(global);
        let entry = FileEntry::new_file("temp.dat".into(), 50, 0o644);
        assert!(chain.allows_entry_deletion(&entry));
    }

    #[test]
    fn chain_scoped_allows_entry() {
        let global = FilterSet::default();
        let mut chain = FilterChain::new(global);

        let scoped = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
        let _guard = chain.push_scope(scoped);

        let excluded = FileEntry::new_file("error.log".into(), 100, 0o644);
        let included = FileEntry::new_file("data.csv".into(), 200, 0o644);
        assert!(!chain.allows_entry(&excluded));
        assert!(chain.allows_entry(&included));
    }

    #[test]
    fn allows_entry_matches_path_based_api() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.o")]).unwrap();
        let entry = FileEntry::new_file("main.o".into(), 4096, 0o644);

        // Generic method and manual extraction must agree.
        let path_result = set.allows(std::path::Path::new(entry.name()), entry.is_dir());
        let entry_result = set.allows_entry(&entry);
        assert_eq!(path_result, entry_result);
        assert!(!entry_result);
    }

    #[test]
    fn allows_entry_first_match_wins() {
        let set = FilterSet::from_rules([
            FilterRule::include("important.bak"),
            FilterRule::exclude("*.bak"),
        ])
        .unwrap();
        let kept = FileEntry::new_file("important.bak".into(), 100, 0o644);
        let dropped = FileEntry::new_file("scratch.bak".into(), 100, 0o644);
        assert!(set.allows_entry(&kept));
        assert!(!set.allows_entry(&dropped));
    }

    #[test]
    fn allows_entry_directory_excluded() {
        let set = FilterSet::from_rules([FilterRule::exclude("build/")]).unwrap();
        let dir = FileEntry::new_directory("build".into(), 0o755);
        assert!(!set.allows_entry(&dir));
    }

    #[test]
    fn allows_entry_directory_included_by_default() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let dir = FileEntry::new_directory("src".into(), 0o755);
        assert!(set.allows_entry(&dir));
    }
}
