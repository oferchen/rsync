//! Bookkeeping for which per-directory merge files are already active.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::NestedDirMerge;

/// Tracks the per-directory merge files already registered for a directory so
/// a directive naming one of them again is discarded rather than re-read.
///
/// upstream: exclude.c:359-375 `add_rule` - "If the local merge file was
/// already mentioned, don't add it again." Upstream holds the active set in
/// the `mergelist_parents` array (exclude.c:174), compares only the BASENAME
/// (`strrchr(rule->pattern, '/')` + 1, exclude.c:354-357), and on a hit
/// silently drops the new rule (`free_filter(rule); return;`).
///
/// Both details are load-bearing:
///
/// * The silence is why `: .rsync-filter` written inside `.rsync-filter` exits
///   0 upstream instead of reporting an error - registering a per-directory
///   merge file is idempotent, not a fault. Measured against rsync 3.5.0.
/// * Without it the per-directory push loop would not terminate. That loop
///   deliberately re-reads `mergelist_cnt` on every iteration so a directive
///   discovered while reading one merge file is honoured for the same
///   directory ("parse_filter_file() might increase mergelist_cnt",
///   exclude.c:885-888); registration is the only thing bounding it.
///
/// Note this is deliberately coarser than path identity: upstream compares
/// basenames, so `sub/.rsync-filter` and `.rsync-filter` are the same merge
/// file to it, and differing modifiers do not make a second registration - the
/// first registration wins and later ones are dropped.
#[derive(Debug, Default)]
pub(crate) struct MergeFileRegistry {
    active: HashSet<OsString>,
}

impl MergeFileRegistry {
    /// Records `pattern` as active, returning `true` when it was not already.
    pub(crate) fn register(&mut self, pattern: &Path) -> bool {
        self.active.insert(merge_file_key(pattern).to_owned())
    }

    /// Drains `incoming` onto `destination`, keeping only rules whose merge
    /// file is not already active.
    pub(crate) fn retain_unregistered(
        &mut self,
        destination: &mut Vec<NestedDirMerge>,
        incoming: &mut Vec<NestedDirMerge>,
    ) {
        for rule in incoming.drain(..) {
            if self.register(&rule.pattern) {
                destination.push(rule);
            }
        }
    }
}

/// The merge-file name upstream compares: everything after the last `/`.
///
/// upstream: exclude.c:354-357 - `if ((cp = strrchr(rule->pattern, '/')) != NULL) cp++; else cp = rule->pattern;`
fn merge_file_key(pattern: &Path) -> &OsStr {
    pattern.file_name().unwrap_or(pattern.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_copy::filter_program::DirMergeOptions;
    use std::path::PathBuf;

    fn rule(pattern: &str) -> NestedDirMerge {
        NestedDirMerge {
            pattern: PathBuf::from(pattern),
            options: DirMergeOptions::default(),
        }
    }

    #[test]
    fn first_registration_wins_and_the_repeat_is_dropped() {
        let mut registry = MergeFileRegistry::default();
        assert!(registry.register(Path::new(".rsync-filter")));
        assert!(!registry.register(Path::new(".rsync-filter")));
    }

    #[test]
    fn distinct_merge_files_both_register() {
        let mut registry = MergeFileRegistry::default();
        assert!(registry.register(Path::new(".rsync-filter")));
        assert!(registry.register(Path::new(".cvsignore")));
    }

    #[test]
    fn comparison_is_by_basename_like_upstream() {
        // upstream: exclude.c:354-357 compares the text after the last '/', so
        // a path-qualified pattern collides with the bare name.
        let mut registry = MergeFileRegistry::default();
        assert!(registry.register(Path::new("sub/.rsync-filter")));
        assert!(!registry.register(Path::new(".rsync-filter")));
    }

    #[test]
    fn self_referential_registration_is_dropped_not_repeated() {
        // The hang case: `.rsync-filter` containing `: .rsync-filter`. The
        // first registration is kept, the self-reference produced by reading
        // it is discarded, so the growing push loop terminates.
        let mut destination = Vec::new();
        let mut registry = MergeFileRegistry::default();
        registry.retain_unregistered(&mut destination, &mut vec![rule(".rsync-filter")]);
        assert_eq!(destination.len(), 1);

        registry.retain_unregistered(&mut destination, &mut vec![rule(".rsync-filter")]);
        assert_eq!(
            destination.len(),
            1,
            "a merge file already active must not be registered again"
        );
    }

    #[test]
    fn mutual_recursion_between_two_files_terminates() {
        let mut destination = Vec::new();
        let mut registry = MergeFileRegistry::default();
        registry.retain_unregistered(&mut destination, &mut vec![rule(".rsync-filter")]);
        registry.retain_unregistered(&mut destination, &mut vec![rule("other.f")]);
        // `other.f` names `.rsync-filter`, which is already active.
        registry.retain_unregistered(&mut destination, &mut vec![rule(".rsync-filter")]);
        assert_eq!(destination.len(), 2);
    }

    #[test]
    fn retain_unregistered_drains_its_input() {
        let mut destination = Vec::new();
        let mut incoming = vec![rule(".rsync-filter"), rule(".rsync-filter")];
        MergeFileRegistry::default().retain_unregistered(&mut destination, &mut incoming);
        assert!(incoming.is_empty());
        assert_eq!(destination.len(), 1);
    }
}
