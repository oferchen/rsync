//! The receiving side's view of a daemon module's filter list.
//!
//! Upstream keeps exactly one `daemon_filter_list` and consults it, per
//! incoming name, at `generator.c:1662-1676`. Its `FILTRULE_PERDIR_MERGE`
//! entries are not flat rules: `check_filter` recurses into
//! `ent->u.mergelist` (`exclude.c:1194-1198`), and that mergelist holds
//! whatever `push_local_filters` last read out of the directory currently
//! being processed (`exclude.c:858-925`). So the same rule list answers
//! differently in each directory, and this module is where the receiver holds
//! that per-directory state.
//!
//! # When the merge files are read
//!
//! On the receiving side the only thing that ever calls
//! `change_local_filter_dir` is the delete pass: `delete_in_dir` calls it for
//! every content directory (`generator.c:314`), and the `delete_during` branch
//! calls it directly for the directories `delete_in_dir` skips
//! (`generator.c:1924-1929`, and `generator.c:2799-2801` under incremental
//! recursion). A transfer without `--delete` reaches none of them, so its
//! mergelist stays empty and a per-directory merge rule is inert - MEASURED
//! against a real rsync 3.5.0 daemon, a push into a module declaring
//! `filter = : .rsync-filter` writes the name `sub/.rsync-filter` excludes
//! unless `--delete` is also passed. [`DaemonFilterGate`] therefore takes the
//! merge configurations only when the delete pass is active; the module's
//! plain rules always apply.

use std::path::{Path, PathBuf};

use filters::{DirMergeConfig, FilterChain, FilterSet};

/// A module's filter list, evaluated in the directory each name lives in.
///
/// Built per receive pass by
/// [`ReceiverContext::daemon_filter_gate`](crate::receiver::ReceiverContext::daemon_filter_gate)
/// and consulted through [`allows`](Self::allows) /
/// [`refuses_ancestor`](Self::refuses_ancestor). Probing takes `&mut self`
/// because a name in a new directory reloads that directory's merge files, the
/// way `change_local_filter_dir` reloads upstream's mergelists.
pub(in crate::receiver) struct DaemonFilterGate {
    /// The module's rules with no directory loaded: the base every
    /// per-directory reload starts from.
    prototype: FilterChain,
    /// Destination root the merge file names are resolved against. This is the
    /// module root for a daemon receiver, matching upstream's post-`chdir`
    /// `curr_dir` in `set_filter_dir()` (`exclude.c:756-776`).
    dest_dir: PathBuf,
    /// Whether any per-directory merge directive is active. False collapses
    /// every probe onto `prototype`, so a module with only plain rules costs
    /// no reloads.
    per_dir: bool,
    /// The directory whose merge files `chain` currently holds, relative to
    /// [`Self::dest_dir`] and `/`-separated, plus that loaded chain.
    loaded: Option<(String, FilterChain)>,
}

impl DaemonFilterGate {
    /// Builds the gate for one receive pass, or `None` when the module
    /// contributes no rules at all.
    ///
    /// `merge_configs` is what the caller decided is live for this pass - empty
    /// when the delete pass is not running, per this module's header.
    pub(in crate::receiver) fn new(
        globals: Option<&FilterSet>,
        merge_configs: &[DirMergeConfig],
        dest_dir: &Path,
    ) -> Option<Self> {
        if globals.is_none() && merge_configs.is_empty() {
            return None;
        }
        let mut prototype = FilterChain::new(globals.cloned().unwrap_or_default());
        for config in merge_configs {
            prototype.add_merge_config(config.clone());
        }
        // upstream: exclude.c:200-228 - a leading-`/` rule read from a per-dir
        // merge file is re-anchored to that file's directory relative to the
        // module root, which is what the transfer root records here.
        prototype.set_transfer_root(dest_dir.to_path_buf());
        Some(Self {
            prototype,
            dest_dir: dest_dir.to_path_buf(),
            per_dir: !merge_configs.is_empty(),
            loaded: None,
        })
    }

    /// Reports whether the module's filter list admits `name`.
    ///
    /// `name` is a wire-format path relative to the destination root, always
    /// `/`-separated. The rules consulted are the module's own plus the merge
    /// files of the directory holding `name` and its ancestors - never
    /// `name`'s own merge file when it is itself a directory, matching
    /// upstream, where `recv_generator` refuses a directory before the
    /// `delete_during` branch below it pushes that directory's filters
    /// (`generator.c:1662` precedes `generator.c:1924-1929`).
    pub(in crate::receiver) fn allows(&mut self, name: &str, is_dir: bool) -> bool {
        let directory = name.rsplit_once('/').map_or("", |(parent, _)| parent);
        self.chain_for(directory).allows(Path::new(name), is_dir)
    }

    /// Reports whether any ancestor directory of `name` is refused, in which
    /// case the entry must be dropped without a diagnostic.
    ///
    /// Upstream refuses the *directory* once, sets `skip_dir` to it, and every
    /// later entry below that directory returns from `recv_generator()` before
    /// the `daemon_filter_list` check is reached - so the contents are dropped
    /// in silence, with no second "daemon refused" line. Recomputing the
    /// ancestor verdict here reproduces that outcome without threading
    /// generator state across oc's separate directory and candidate passes.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1258-1266` - `if (skip_dir) { if (is_below(file, skip_dir)) ... return; }`
    /// - `generator.c:1284-1285` / `generator.c:1491-1495` - a refused directory
    ///   jumps to `skipping_dir_contents`, which assigns `skip_dir = file`
    pub(in crate::receiver) fn refuses_ancestor(&mut self, name: &str) -> bool {
        let mut cursor = name;
        while let Some(separator) = cursor.rfind('/') {
            cursor = &cursor[..separator];
            if cursor.is_empty() {
                break;
            }
            if !self.allows(cursor, true) {
                return true;
            }
        }
        false
    }

    /// Returns the rule chain as it stands inside `directory`, reloading that
    /// directory's merge files when the previous probe was in another one.
    ///
    /// upstream: `exclude.c:974-1000 change_local_filter_dir()` pops the merge
    /// state above the new depth and pushes the new directory's. oc's candidate
    /// and directory passes do not descend in a single ordered walk, so the
    /// state is rebuilt from the destination root instead of popped down to a
    /// depth; the loaded rules are the same either way.
    fn chain_for(&mut self, directory: &str) -> &FilterChain {
        if !self.per_dir {
            return &self.prototype;
        }
        if self
            .loaded
            .as_ref()
            .is_none_or(|(loaded, _)| loaded != directory)
        {
            let mut chain = self.prototype.clone();
            chain.reload_for_directory(&self.dest_dir, Path::new(directory));
            self.loaded = Some((directory.to_owned(), chain));
        }
        &self
            .loaded
            .as_ref()
            .expect("the directory was just loaded")
            .1
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use filters::{DirMergeConfig, FilterRule, FilterSet};
    use tempfile::TempDir;

    use super::DaemonFilterGate;

    /// A destination whose ONLY mention of `bait.txt` is inside
    /// `sub/.rsync-filter`, so nothing but a real read of that file can refuse
    /// the name.
    fn dest_with_merge_file() -> TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sub = tmp.path().join("sub");
        fs::create_dir_all(&sub).expect("create sub");
        fs::write(sub.join(".rsync-filter"), b"- bait.txt\n").expect("write merge file");
        tmp
    }

    fn merge_configs() -> Vec<DirMergeConfig> {
        vec![DirMergeConfig::new(".rsync-filter")]
    }

    /// The discriminating cell.
    ///
    /// Ground truth, MEASURED against a real rsync 3.5.0 daemon on loopback TCP
    /// with `read only = no` and a module declaring `filter = : .rsync-filter`,
    /// pushing `sub/bait.txt` and `sub/keep.txt` with `--delete`: the daemon
    /// answers `ERROR: daemon refused to receive file "sub/bait.txt"` and exits
    /// 23, while `sub/keep.txt` lands.
    ///
    /// ⚠ The load-bearing half is the pair. A gate that refused everything
    /// under `sub/` would satisfy the bait assertion on its own, so the sibling
    /// control is asserted with it.
    #[test]
    fn a_dir_merge_refuses_the_name_only_the_merge_file_names() {
        let dest = dest_with_merge_file();
        let mut gate = DaemonFilterGate::new(None, &merge_configs(), dest.path())
            .expect("a merge directive alone builds a gate");

        assert!(
            !gate.allows("sub/bait.txt", false),
            "sub/.rsync-filter excludes bait.txt, so the receiver must refuse it"
        );
        assert!(
            gate.allows("sub/keep.txt", false),
            "the sibling control must not be swept up"
        );
    }

    /// The merge file governs its own directory, not the whole tree.
    ///
    /// Upstream reads `sub/.rsync-filter` while processing `sub`, so a
    /// same-named file at the destination root is judged by the root's rules -
    /// of which there are none here. Without this the first test would also
    /// pass on a gate that simply applied every merge file everywhere.
    #[test]
    fn a_merge_file_does_not_reach_outside_its_directory() {
        let dest = dest_with_merge_file();
        let mut gate = DaemonFilterGate::new(None, &merge_configs(), dest.path())
            .expect("a merge directive alone builds a gate");

        assert!(
            gate.allows("bait.txt", false),
            "a root-level name is not governed by sub/.rsync-filter"
        );
        assert!(
            !gate.allows("sub/bait.txt", false),
            "the same name inside sub/ still is"
        );
    }

    /// Directory changes both ways, so the loaded-directory cache cannot pass
    /// by loading once and never reloading, nor by never caching a miss.
    #[test]
    fn the_verdict_follows_the_directory_back_and_forth() {
        let dest = dest_with_merge_file();
        let mut gate = DaemonFilterGate::new(None, &merge_configs(), dest.path())
            .expect("a merge directive alone builds a gate");

        assert!(!gate.allows("sub/bait.txt", false));
        assert!(gate.allows("bait.txt", false));
        assert!(!gate.allows("sub/bait.txt", false));
        assert!(gate.allows("bait.txt", false));
    }

    /// With no merge directive live - which is what the gate is handed when
    /// `--delete` is absent - the module's plain rules still apply and the
    /// merge file on disk is not consulted.
    ///
    /// MEASURED: pushing into the same module WITHOUT `--delete`, real rsync
    /// 3.5.0 writes `sub/bait.txt` and exits 0, because nothing on the
    /// receiving side calls `change_local_filter_dir` outside the delete pass.
    #[test]
    fn without_merge_configs_the_merge_file_is_not_read() {
        let dest = dest_with_merge_file();
        let globals =
            FilterSet::from_rules(vec![FilterRule::exclude("plain.txt")]).expect("compiles");
        let mut gate = DaemonFilterGate::new(Some(&globals), &[], dest.path())
            .expect("plain rules alone build a gate");

        assert!(
            gate.allows("sub/bait.txt", false),
            "the merge file must stay unread when no merge directive is live"
        );
        assert!(
            !gate.allows("plain.txt", false),
            "the module's plain rules apply with or without the delete pass"
        );
    }

    /// A module that declares nothing has no gate, so the passes skip the probe
    /// entirely.
    #[test]
    fn an_empty_module_builds_no_gate() {
        let dest = dest_with_merge_file();
        assert!(DaemonFilterGate::new(None, &[], dest.path()).is_none());
    }
}
