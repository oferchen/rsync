//! CVE-2026-53786 (daemon-filter-merge-bypass): a pushing client must not be
//! able to make the daemon read a file its module filter hides by naming that
//! file as a per-directory merge file.
//!
//! The client sends `: secret` (a dir-merge rule) over the filter list and
//! pushes with `--delete`. In the module, `secret` holds `- decoy`, and
//! `decoy` exists only on the destination. If the daemon reads `secret`, that
//! rule excludes `decoy` from deletion and it survives; if the daemon treats
//! the hidden merge file as absent, `--delete` removes `decoy`. So the
//! survival of `decoy` is a direct observation of whether the hidden file was
//! read. Each gated scenario has an ungated control in which `decoy` must
//! survive, proving the oracle discriminates rather than passing vacuously.
//!
//! upstream: exclude.c:1638-1659 parse_filter_file() - every merge-file open
//! is checked against `daemon_filter_list` on the module-relative path; a
//! hidden file is treated as non-existent, with no error and no
//! fatal-vs-silent oracle. MEASURED against rsync 3.5.1: all five scenarios
//! below behave as asserted, exit 0.
#![cfg(unix)]
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use test_support::{DaemonProcess, oc_rsync_bin};
/// One push into a daemon module whose destination holds `decoy` and a
/// `secret` merge file containing `- decoy`.
struct Scenario<'a> {
    /// Module directive lines appended to the `[mod]` section.
    directive: &'a str,
    /// Directory under the module root the client pushes into (`""` = root).
    target: &'a str,
    /// Directory below the transfer root that holds `secret` and `decoy`.
    nested: &'a str,
}
/// Runs the scenario and reports whether `decoy` survived the push.
fn decoy_survives(scenario: &Scenario<'_>) -> bool {
    let temp = TempDir::new().expect("tempdir");
    let src = temp.path().join("src");
    let module = temp.path().join("mod");
    let dest = module.join(scenario.target).join(scenario.nested);
    fs::create_dir_all(src.join(scenario.nested)).expect("create src");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(src.join(scenario.nested).join("keep"), b"payload\n").expect("write src keep");
    fs::write(dest.join("keep"), b"payload\n").expect("write dest keep");
    fs::write(dest.join("decoy"), b"decoy\n").expect("write decoy");
    fs::write(dest.join("secret"), b"- decoy\n").expect("write secret");
    let config = temp.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "log file = {}\n[mod]\n    path = {}\n    use chroot = false\n    read only = false\n    {}\n",
            temp.path().join("rsyncd.log").display(),
            module.display(),
            scenario.directive
        ),
    )
    .expect("write rsyncd.conf");
    let daemon = DaemonProcess::spawn(&config, std::iter::empty::<&str>()).expect("spawn daemon");
    let target = if scenario.target.is_empty() {
        String::new()
    } else {
        format!("{}/", scenario.target)
    };
    let output = Command::new(oc_rsync_bin())
        .arg("-r")
        .arg("--delete")
        .arg("--filter=: secret")
        .arg(format!("{}/", src.display()))
        .arg(format!("rsync://127.0.0.1:{}/mod/{target}", daemon.port()))
        .env("OC_RSYNC_FALLBACK", "0")
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "push must succeed like upstream (exit 0): status {:?}, stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dest.join("keep").is_file(),
        "the pushed file must land, or the transfer did nothing"
    );
    path_exists(&dest.join("decoy"))
}
fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
#[test]
fn ungated_merge_file_is_read_control() {
    assert!(
        decoy_survives(&Scenario {
            directive: "",
            target: "",
            nested: ""
        }),
        "control: with no daemon filter the client's merge file must be read, \
         keeping decoy; otherwise the oracle below cannot tell gated from broken"
    );
}
#[test]
fn exclude_hides_merge_file_from_client_dir_merge() {
    assert!(
        !decoy_survives(&Scenario {
            directive: "exclude = secret",
            target: "",
            nested: ""
        }),
        "the daemon read `secret`, which `exclude = secret` hides (CVE-2026-53786)"
    );
}
#[test]
fn filter_rule_hides_merge_file_from_client_dir_merge() {
    assert!(
        !decoy_survives(&Scenario {
            directive: "filter = - secret",
            target: "",
            nested: ""
        }),
        "the daemon read `secret`, which `filter = - secret` hides (CVE-2026-53786)"
    );
}
#[test]
fn nested_merge_file_is_hidden_too() {
    assert!(
        !decoy_survives(&Scenario {
            directive: "exclude = secret",
            target: "",
            nested: "a"
        }),
        "the daemon read `a/secret`, which `exclude = secret` hides (CVE-2026-53786)"
    );
}
#[test]
fn anchored_rule_is_matched_on_the_module_relative_path() {
    assert!(
        decoy_survives(&Scenario {
            directive: "",
            target: "sub",
            nested: ""
        }),
        "control: pushing into mod/sub/ with no daemon filter must read sub/secret"
    );
    assert!(
        !decoy_survives(&Scenario {
            directive: "exclude = /sub/secret",
            target: "sub",
            nested: ""
        }),
        "the daemon read `sub/secret`, which the module-anchored `exclude = /sub/secret` \
         hides; the gate must match the module-relative path (exclude.c:1640-1647)"
    );
}
