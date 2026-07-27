//! The generator's destination xattr read honours `x`-modifier filter rules.
//!
//! upstream: `xattrs.c:250-257` - `saw_xattr_filter` is a global consulted
//! inside `rsync_xal_get()` on *every* call, so it screens the generator's
//! destination read exactly as it screens the sender's source read. An
//! excluded name is therefore invisible to `xattr_diff()` on both sides and
//! cannot raise the itemize `x` column.

use protocol::flist::FileEntry;
use tempfile::TempDir;

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};

/// The on-disk spelling the filter has to match: upstream feeds
/// `name_is_excluded()` the *local* name it read from `listxattr`
/// (`xattrs.c:246-251`). Linux exposes namespaced names, every other unix a
/// flat one, so the `user.` prefix is Linux-only.
fn local_name(base: &str) -> String {
    if cfg!(target_os = "linux") {
        format!("user.{base}")
    } else {
        base.to_owned()
    }
}

/// Materialises `base` on `path` so the destination read has something to see.
/// Returns `false` when the filesystem cannot store it, so a caller can skip
/// rather than fail on a backend without xattr support.
fn set_dest_xattr(path: &std::path::Path, base: &str, value: &[u8]) -> bool {
    xattr::set(path, local_name(base), value).is_ok()
}

/// Screens out attributes the operating system attaches on its own, so the
/// assertions below turn on the one name the test controls.
///
/// macOS stamps `com.apple.provenance` onto every newly created file, which
/// would otherwise leave the destination list non-empty no matter what the
/// rule under test does. It is inert on platforms that add nothing.
fn platform_noise_rule() -> ::filters::FilterRule {
    ::filters::FilterRule::exclude("com.apple.*").with_xattr_only(true)
}

/// Builds a receiver whose global filter set carries `rules`.
fn receiver_with(rules: Vec<::filters::FilterRule>) -> ReceiverContext {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.xattrs = true;
    config.flags.xattrs_level = 1;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    let global = ::filters::FilterSet::from_rules(rules).expect("compile filter rules");
    ctx.set_filter_chain(::filters::FilterChain::new(global));
    ctx
}

/// An `x`-modifier exclusion hides the name from the destination read, so a
/// destination-only attribute no longer counts as a difference.
///
/// This is the behaviour under test: without the filter on the generator's
/// read, the excluded attribute survives on the destination list alone and
/// `xattr_diff()` reports a change that upstream never reports - and that no
/// transfer could settle, since the same filter stops the receiver from ever
/// removing it (`xattrs.c:1026`).
#[test]
fn an_excluded_name_does_not_flip_the_x_column() {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("f.txt");
    std::fs::write(&file, b"content").expect("write file");
    if !set_dest_xattr(&file, "drop", b"DSTVAL") {
        eprintln!("xattrs unsupported here, skipping");
        return;
    }

    let entry = FileEntry::new_file("f.txt".into(), 7, 0o644);

    // Control: with the platform noise screened but no rule naming it, the
    // destination-only attribute is a difference, exactly as `rsync -X -i`
    // reports `.f........x`. Without this the negative assertion below could
    // pass for the wrong reason.
    let unfiltered = receiver_with(vec![platform_noise_rule()]);
    assert!(
        unfiltered.dest_xattrs_differ(&entry, Some(&file)),
        "a destination-only xattr must raise the x column when nothing excludes it",
    );

    // `--filter '-x <name>'`: upstream drops the name inside rsync_xal_get()
    // before it can reach xattr_diff(), so no `x` column is emitted.
    let filtered = receiver_with(vec![
        ::filters::FilterRule::exclude(local_name("drop")).with_xattr_only(true),
        platform_noise_rule(),
    ]);
    assert!(
        !filtered.dest_xattrs_differ(&entry, Some(&file)),
        "an x-modifier exclusion must screen the generator's destination read",
    );
}

/// The filter is not a blanket suppression: a name it admits still differs.
///
/// Guards the opposite failure mode - screening the destination read with a
/// filter that happens to be present, rather than with the rule that actually
/// matches, would silence real xattr changes and make `-X` a no-op whenever
/// any `x` rule existed.
#[test]
fn a_name_the_filter_admits_still_flips_the_x_column() {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("f.txt");
    std::fs::write(&file, b"content").expect("write file");
    if !set_dest_xattr(&file, "drop", b"DSTVAL") {
        eprintln!("xattrs unsupported here, skipping");
        return;
    }

    let entry = FileEntry::new_file("f.txt".into(), 7, 0o644);

    // The rule names a different attribute, so `user.drop` is still collected.
    // Everything the platform adds on its own is screened, which leaves that
    // one attribute as the sole possible source of the difference.
    let ctx = receiver_with(vec![
        ::filters::FilterRule::exclude(local_name("other")).with_xattr_only(true),
        platform_noise_rule(),
    ]);
    assert!(
        ctx.dest_xattrs_differ(&entry, Some(&file)),
        "an unrelated x-modifier rule must not suppress a real xattr difference",
    );
}

/// A rule *without* the `x` modifier governs paths only and must leave the
/// destination read alone.
///
/// upstream: `exclude.c:914` - `rule_matches()` gates every rule on
/// `!(name_flags & NAME_IS_XATTR) ^ !(ex->rflags & FILTRULE_XATTR)`, and
/// `saw_xattr_filter` is set only by `x`-modifier rules, so a plain `-` rule
/// never reaches `rsync_xal_get()`.
#[test]
fn a_plain_path_rule_does_not_screen_the_destination_read() {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("f.txt");
    std::fs::write(&file, b"content").expect("write file");
    if !set_dest_xattr(&file, "drop", b"DSTVAL") {
        eprintln!("xattrs unsupported here, skipping");
        return;
    }

    let entry = FileEntry::new_file("f.txt".into(), 7, 0o644);

    let ctx = receiver_with(vec![::filters::FilterRule::exclude(local_name("drop"))]);
    assert!(
        ctx.dest_xattrs_differ(&entry, Some(&file)),
        "a rule without the x modifier matches paths, never xattr names",
    );
}
