//! Receiver-side re-filter of received file-list names.
//!
//! A receiver must never be forced to materialize a path its own filter rules
//! exclude. Upstream `recv_file_entry()` (flist.c:1019-1030) re-runs every
//! received name through `check_server_filter` and aborts with
//! `RERR_UNSUPPORTED` when a sender smuggles an excluded name into the list.
//! These tests encode that invariant for both the server-receiver
//! (`filter_chain` built from the wire rules) and the local-client pull
//! (`config.connection.filter_rules`), and prove the upstream skip conditions
//! (`trust_sender`, per-directory merge) as well as the `--delete-excluded`
//! interaction (it governs local deletion, not what a sender may place in the
//! file list).

use std::io;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};
use protocol::filters::FilterRuleWireFormat;
use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

fn chain_excluding(pattern: &str) -> FilterChain {
    let global = FilterSet::from_rules([FilterRule::exclude(pattern)]).unwrap();
    FilterChain::new(global)
}

#[test]
fn excluded_name_rejected_via_filter_chain() {
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.set_filter_chain(chain_excluding("*.log"));
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 10, 0o644));
    ctx.file_list
        .push(FileEntry::new_file("debug.log".into(), 20, 0o644));

    let err = ctx.recheck_received_filter().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting excluded file-list name: debug.log"
    );
}

#[test]
fn included_names_pass_via_filter_chain() {
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.set_filter_chain(chain_excluding("*.log"));
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 10, 0o644));
    ctx.file_list
        .push(FileEntry::new_directory("subdir".into(), 0o755));

    ctx.recheck_received_filter()
        .expect("names allowed by the filter chain must pass the re-check");
}

#[test]
fn trust_sender_skips_recheck() {
    // upstream: flist.c:1021 - `!trust_sender_filter` guards the re-check; a
    // local transfer or `--trust-sender` skips it entirely.
    let mut config = test_config();
    config.trust_sender = true;
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.set_filter_chain(chain_excluding("*.log"));
    ctx.file_list
        .push(FileEntry::new_file("debug.log".into(), 20, 0o644));

    ctx.recheck_received_filter()
        .expect("trust_sender must skip the receiver-side re-check");
}

#[test]
fn empty_filter_chain_is_a_no_op() {
    // No receiver-owned rules: a normal transfer must not be disturbed.
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.file_list
        .push(FileEntry::new_file("anything.log".into(), 20, 0o644));

    ctx.recheck_received_filter()
        .expect("an empty filter chain re-checks nothing");
}

#[test]
fn per_dir_merge_defers_to_sender() {
    // upstream: exclude.c:1182 - a per-directory merge (`:`) rule sets
    // trust_sender_filter, so the receiver cannot reproduce the sender's
    // per-directory decision and must trust the sender's filtering.
    let mut chain = chain_excluding("*.log");
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
    assert!(chain.has_per_dir_merge());

    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.set_filter_chain(chain);
    ctx.file_list
        .push(FileEntry::new_file("debug.log".into(), 20, 0o644));

    ctx.recheck_received_filter()
        .expect("a per-directory merge rule defers filtering to the sender");
}

#[test]
fn client_pull_excluded_name_rejected() {
    // Local-client pull: the client never reads a wire filter list, so its own
    // CLI rules live in `config.connection.filter_rules`. The receiver must
    // still reject an excluded name the remote sender should not have sent.
    let mut config = test_config();
    config.connection.client_mode = true;
    config.connection.filter_rules = vec![FilterRuleWireFormat::exclude("*.log".to_owned())];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("secret.log".into(), 20, 0o644));

    let err = ctx.recheck_received_filter().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(
        err.to_string(),
        "ERROR: rejecting excluded file-list name: secret.log"
    );
}

#[test]
fn delete_excluded_does_not_weaken_recheck() {
    // upstream: check_server_filter (exclude.c:1030, LOCAL_RULE) evaluates the
    // sender-side rules regardless of --delete-excluded. --delete-excluded
    // governs which local destination files may be deleted, not which names a
    // sender may place in the file list, so an excluded name is still rejected.
    let mut config = test_config();
    config.deletion.delete_excluded = true;
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.set_filter_chain(chain_excluding("*.log"));
    ctx.file_list
        .push(FileEntry::new_file("x.log".into(), 20, 0o644));

    let err = ctx.recheck_received_filter().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn anchored_dir_exclude_does_not_reject_included_children() {
    // Regression for the upstream `exclude-lsh` testsuite: the `$excl` file
    // pairs `+ **/bar` with `- /bar`. The sender includes `bar` (via `+ **/bar`)
    // and legitimately sends its children. Upstream `check_server_filter` ->
    // `rule_matches` strips a slashless anchored pattern (`bar`) to the basename
    // before matching, so `bar/down` compares `bar` against `down` -> no match
    // -> accepted. The re-check must mirror that (no synthetic `bar/**`
    // descendant matcher) and NOT abort on the included children.
    let global =
        FilterSet::from_rules([FilterRule::include("**/bar"), FilterRule::exclude("/bar")])
            .unwrap();
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.set_filter_chain(FilterChain::new(global));
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("bar".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("bar/down".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("bar/down/to/file".into(), 10, 0o644));

    ctx.recheck_received_filter().expect(
        "children under an included parent must not be rejected by a slashless \
         anchored dir exclude (upstream rule_matches has no descendant matching)",
    );
}

#[test]
fn directory_only_wire_rule_does_not_reject_files_under_it() {
    // Regression for the upstream `exclude-lsh` testsuite (client pull): the
    // `$excl` file pairs `+ **/bar` with `- foo/*/`. The CLI encodes `foo/*/`
    // onto the wire as pattern `foo/*` + directory_only=true (flags.rs
    // split_pattern_modifiers). The receiver must rebuild the rule as
    // directory-only so it matches only foo's SUBDIRECTORIES, never a plain
    // file like `bar/down/to/foo/+ file3` that the sender legitimately sent.
    // Before honoring `directory_only`, the receiver's `foo/*` matched the
    // file and aborted with RERR_UNSUPPORTED.
    let mut config = test_config();
    config.connection.client_mode = true;
    config.connection.filter_rules = vec![
        FilterRuleWireFormat::include("**/bar".to_owned()),
        FilterRuleWireFormat::exclude("foo/*".to_owned()).with_directory_only(true),
    ];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("bar/down/to/foo".into(), 0o755));
    // A regular file directly under `foo` - a dir-only rule must NOT match it.
    ctx.file_list.push(FileEntry::new_file(
        "bar/down/to/foo/+ file3".into(),
        6,
        0o644,
    ));

    ctx.recheck_received_filter().expect(
        "a directory-only exclude (foo/*/) must not reject a plain file under foo \
         (upstream exclude-lsh keeps bar/down/to/foo/+ file3)",
    );
}

#[test]
fn directory_only_wire_rule_still_rejects_a_smuggled_directory() {
    // The dir-only fix must not disable the guard: a directory the rule DOES
    // exclude (a subdirectory of foo) is still rejected if a sender smuggles it
    // into the list. Mirrors upstream keeping `- foo/*/` effective for dirs.
    let mut config = test_config();
    config.connection.client_mode = true;
    config.connection.filter_rules =
        vec![FilterRuleWireFormat::exclude("foo/*".to_owned()).with_directory_only(true)];
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("foo/secret".into(), 0o755));

    let err = ctx.recheck_received_filter().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn transfer_root_never_rejected() {
    // upstream: flist.c:1019 - the transfer root (`.`) is exempt from the
    // re-check even when a catch-all exclude would otherwise match it.
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    ctx.set_filter_chain(chain_excluding("*"));
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));

    ctx.recheck_received_filter()
        .expect("the transfer root must never be rejected by the re-check");
}
