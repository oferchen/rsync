//! The `/` modifier on a `dir-merge` directive is a MATCHING flag, not a
//! pattern rewrite.
//!
//! Upstream sets `FILTRULE_ABS_PATH` on the dir-merge rule when the directive
//! carries the `/` modifier (`exclude.c:1392-1394`), and the flag is a member of
//! `FILTRULES_FROM_CONTAINER` (`exclude.c:1229-1231`), so `parse_rule_tok`
//! copies it onto every record the merge file yields
//! (`exclude.c:1261-1263`). Its ONLY effect on the stored rule is negative:
//! `add_rule` refuses to prepend the merge file's own directory when the flag is
//! already set (`exclude.c:297-305`), which is the re-anchoring that
//! `XFLG_ANCHORED2ABS` otherwise performs for every per-directory load
//! (`exclude.c:911-913`). The pattern TEXT is never touched, so an unanchored
//! pattern keeps its unanchored matching: `exclude.c:1016-1021` takes the
//! basename branch for a slash-free pattern regardless of `FILTRULE_ABS_PATH`.
//!
//! Why this matters: oc used to spell the modifier as "prepend `/` to the
//! pattern". That silently converts every slash-free rule in the merge file from
//! a match-at-any-depth rule into a match-at-the-transfer-root-only rule, so a
//! `:/ .filt` holding `- bar` stops excluding anything below the merge file's
//! own directory. The expectations below are the measured output of upstream
//! rsync 3.5.0 on the matching fixture, not a restatement of oc's code.

use filters::{DirMergeConfig, FilterChain, FilterSet};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Builds `root/{top,bar,sub/{bar,keep,deeper/bar}}` plus `sub/.filt` holding
/// `body`, and returns a chain rooted at `root` with one dir-merge config.
fn chain_for(body: &str, anchor_root: bool) -> (TempDir, FilterChain) {
    let root = TempDir::new().expect("tempdir");
    let sub = root.path().join("sub");
    fs::create_dir_all(sub.join("deeper")).expect("mkdir");
    for rel in ["top", "bar", "sub/bar", "sub/keep", "sub/deeper/bar"] {
        fs::write(root.path().join(rel), b"").expect("write fixture file");
    }
    fs::write(sub.join(".filt"), body).expect("write merge file");

    let mut chain = FilterChain::new(FilterSet::default());
    chain.set_transfer_root(root.path());
    chain.add_merge_config(DirMergeConfig::new(".filt").with_anchor_root(anchor_root));
    (root, chain)
}

/// Walks into `sub` (where the merge file lives) and reports, for each fixture
/// path below it, whether the chain still allows the transfer.
fn verdicts_under_sub(root: &Path, chain: &mut FilterChain) -> Vec<(&'static str, bool)> {
    let root_guard = chain.enter_directory(root).expect("enter root");
    let sub_guard = chain.enter_directory(&root.join("sub")).expect("enter sub");
    let verdicts = ["sub/bar", "sub/keep", "sub/deeper/bar"]
        .into_iter()
        .map(|rel| (rel, chain.allows(Path::new(rel), false)))
        .collect();
    chain.leave_directory(sub_guard);
    chain.leave_directory(root_guard);
    verdicts
}

/// Measured on rsync 3.5.0: `:/ .filt` with `- bar` in `sub/.filt` excludes both
/// `sub/bar` and `sub/deeper/bar`. `bar` has no slash, so `rule_matches` takes
/// the basename branch (`exclude.c:1017-1021`) and `FILTRULE_ABS_PATH` never
/// enters the decision. Rewriting the pattern to `/bar` breaks both cells.
#[test]
fn abs_path_modifier_leaves_a_slash_free_pattern_matching_at_every_depth() {
    let (root, mut chain) = chain_for("- bar\n", true);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", false),
            ("sub/keep", true),
            ("sub/deeper/bar", false),
        ],
        "`:/` must not anchor a slash-free merge-file rule to the transfer root",
    );
}

/// Measured on rsync 3.5.0: `:/ .filt` with `- deeper/bar` excludes
/// `sub/deeper/bar` and leaves `sub/bar`. An unanchored pattern with one infix
/// slash matches the last two path elements (`exclude.c:1046-1049`
/// `slash_handling = ex->u.slash_cnt + 1`). Prepending `/` turns it into a
/// transfer-root anchor that matches nothing under `sub/`.
#[test]
fn abs_path_modifier_keeps_an_infix_slash_pattern_tail_matching() {
    let (root, mut chain) = chain_for("- deeper/bar\n", true);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", true),
            ("sub/keep", true),
            ("sub/deeper/bar", false),
        ],
        "`:/` must leave an unanchored infix-slash rule matching by path tail",
    );
}

/// Measured on rsync 3.5.0: with `+ /bar` ahead of `- bar` under `:/`, the
/// anchored include matches nothing below `sub/` and the unanchored exclude
/// still takes both `bar` files. Guards the include half of the same rewrite:
/// an anchored `+` must not be able to rescue what the rewrite would have
/// anchored alongside it.
#[test]
fn abs_path_modifier_include_stays_anchored_while_exclude_stays_free() {
    let (root, mut chain) = chain_for("+ /bar\n- bar\n", true);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", false),
            ("sub/keep", true),
            ("sub/deeper/bar", false),
        ],
        "an anchored `+ /bar` under `:/` must not shadow the unanchored `- bar`",
    );
}

/// Negative control. Measured on rsync 3.5.0: `:/ .filt` with `- /bar` excludes
/// nothing under `sub/`, because skipping the merge-directory prefix leaves the
/// anchor bound above `sub/`. Already-anchored patterns were the one case the
/// old rewrite got right, so this cell must stay green across the fix.
#[test]
fn abs_path_modifier_skips_the_merge_directory_reanchor() {
    let (root, mut chain) = chain_for("- /bar\n", true);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", true),
            ("sub/keep", true),
            ("sub/deeper/bar", true),
        ],
        "`:/` must suppress the merge-directory re-anchor of `- /bar`",
    );
}

/// Negative control. Without the `/` modifier, `- /bar` in `sub/.filt` IS
/// re-anchored to the merge file's directory (`exclude.c:297-305` computes
/// `pre_len` from `dirbuf`), so it excludes `sub/bar` and nothing deeper.
/// Measured on rsync 3.5.0.
#[test]
fn plain_dir_merge_reanchors_an_anchored_rule_to_the_merge_directory() {
    let (root, mut chain) = chain_for("- /bar\n", false);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", false),
            ("sub/keep", true),
            ("sub/deeper/bar", true),
        ],
        "a plain `:` must re-anchor `- /bar` onto the merge file's directory",
    );
}

/// Negative control. Without the `/` modifier a slash-free rule already matched
/// at every depth; the fix must not change it. Measured on rsync 3.5.0.
#[test]
fn plain_dir_merge_leaves_a_slash_free_pattern_alone() {
    let (root, mut chain) = chain_for("- bar\n", false);
    assert_eq!(
        verdicts_under_sub(root.path(), &mut chain),
        vec![
            ("sub/bar", false),
            ("sub/keep", true),
            ("sub/deeper/bar", false),
        ],
        "a plain `:` must leave a slash-free merge-file rule unanchored",
    );
}

/// The `/` modifier must not disturb the other modifiers `apply_modifiers`
/// owns. `:/sr`-style combinations still have to reach the parsed rule, so a
/// fix that removes the anchor rewrite must remove ONLY that.
#[test]
fn abs_path_modifier_composes_with_the_side_and_perishable_modifiers() {
    let (root, mut chain) = TempDir::new()
        .map(|root| {
            let sub = root.path().join("sub");
            fs::create_dir_all(&sub).expect("mkdir");
            fs::write(sub.join("bar"), b"").expect("write");
            fs::write(sub.join(".filt"), "- bar\n").expect("write merge file");
            let mut chain = FilterChain::new(FilterSet::default());
            chain.set_transfer_root(root.path());
            chain.add_merge_config(
                DirMergeConfig::new(".filt")
                    .with_anchor_root(true)
                    .with_receiver_only(true),
            );
            (root, chain)
        })
        .expect("tempdir");

    let root_guard = chain.enter_directory(root.path()).expect("enter root");
    let sub_guard = chain
        .enter_directory(&root.path().join("sub"))
        .expect("enter sub");
    // A receiver-only rule is inert on the sender's transfer decision but still
    // governs the receiver's delete pass.
    assert!(
        chain.allows(Path::new("sub/bar"), false),
        "a receiver-only `- bar` must not exclude on the sender side",
    );
    assert!(
        !chain.allows_deletion(Path::new("sub/bar"), false),
        "a receiver-only `- bar` must still protect sub/bar from deletion",
    );
    chain.leave_directory(sub_guard);
    chain.leave_directory(root_guard);
}
