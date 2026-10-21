//! Oracle tests for the option values a client passes to a remote-shell server.
//!
//! The expected argv slices were recorded from upstream rsync 3.5.1 with a
//! recording rsh (`-e` pointing at a script that logs its argv) for the same
//! options. A remote shell re-splits the command line, so a value must carry
//! exactly upstream's escaping and split form to reach the server intact: an
//! unescaped space splits it, an unescaped `*` globs on the remote host, and a
//! joined `--backup-dir=~/x` defeats the `~` expansion the split form exists
//! for (options.c:2982-2984).

use std::ffi::{OsStr, OsString};

use super::RemoteRole;
use super::builder::RemoteInvocationBuilder;
use crate::client::config::{ClientConfig, FilesFromSource, StrongChecksumChoice};

fn push_config(old_args: Option<u8>) -> ClientConfig {
    ClientConfig::builder()
        .backup(true)
        .backup_directory(Some("~/bak dir*"))
        .backup_suffix(Some(".b k*"))
        .partial_directory(Some(".p d\"q"))
        .temp_directory(Some("/t'm p"))
        .compare_destination("c\\d[x]")
        .user_mapping(Some(
            ::metadata::UserMapping::parse("*:nobody").expect("usermap"),
        ))
        .group_mapping(Some(
            ::metadata::GroupMapping::parse("*:nogroup").expect("groupmap"),
        ))
        .remote_options(["--log-file=/l g*"])
        .old_args(old_args)
        .build()
}

fn push_argv(config: &ClientConfig) -> Vec<String> {
    lossy(RemoteInvocationBuilder::new(config, RemoteRole::Sender).build(OsStr::new("dst")))
}

fn lossy(args: Vec<OsString>) -> Vec<String> {
    args.into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Asserts each upstream slice appears contiguously in `argv`. The order of
/// the slices is not compared: oc emits the long options in its own order.
fn assert_has_slices(argv: &[String], slices: &[&[&str]]) {
    for slice in slices {
        assert!(
            argv.windows(slice.len()).any(|w| w == *slice),
            "missing upstream argv slice {slice:?} in {argv:?}"
        );
    }
}

/// `rsync -e REC -a -b --backup-dir='~/bak dir*' --suffix='.b k*'
/// --partial-dir='.p d"q' --temp-dir="/t'm p" --compare-dest='c\d[x]'
/// --usermap='*:nobody' --groupmap='*:nogroup' -M'--log-file=/l g*' src/ host:dst`
const PUSH_351: &[&[&str]] = &[
    &["--backup-dir", "~/bak\\ dir\\*"],
    &["--suffix=.b\\ k\\*"],
    &["--partial-dir", ".p\\ d\\\"q"],
    &["--usermap=\\*:nobody"],
    &["--groupmap=\\*:nogroup"],
    &["--temp-dir", "/t\\'m\\ p"],
    &["--compare-dest", "c\\\\d\\[x\\]"],
    &["--log-file=/l\\ g\\*"],
];

#[test]
fn push_option_values_match_upstream_argv() {
    assert_has_slices(&push_argv(&push_config(None)), PUSH_351);
}

/// `--old-args` (level 1) still escapes option values but leaves `-M`
/// remote options for the remote shell to split (`SPLIT_ARG_WHEN_OLD`).
#[test]
fn old_args_level_one_leaves_remote_options_unescaped_like_upstream() {
    let argv = push_argv(&push_config(Some(1)));
    let mut expected = PUSH_351[..PUSH_351.len() - 1].to_vec();
    expected.push(&["--log-file=/l g*"]);
    assert_has_slices(&argv, &expected);
}

/// `--old-args --old-args` (level 2) escapes nothing; the split forms stay.
#[test]
fn old_args_level_two_escapes_nothing_like_upstream() {
    assert_has_slices(
        &push_argv(&push_config(Some(2))),
        &[
            &["--backup-dir", "~/bak dir*"],
            &["--suffix=.b k*"],
            &["--partial-dir", ".p d\"q"],
            &["--usermap=*:nobody"],
            &["--groupmap=*:nogroup"],
            &["--temp-dir", "/t'm p"],
            &["--compare-dest", "c\\d[x]"],
            &["--log-file=/l g*"],
        ],
    );
}

/// `rsync -e REC -az --skip-compress='gz/j*g' --files-from=':/f rom~'
/// --checksum-choice=md5 -M'--log-file=/l g*' 'host:src dir' pdst`
#[test]
fn pull_option_values_match_upstream_argv() {
    let config = ClientConfig::builder()
        .compress(true)
        .skip_compress_spec(Some("gz/j*g".to_owned()))
        .files_from(FilesFromSource::RemoteFile("/f rom~".to_owned()))
        .checksum_choice(StrongChecksumChoice::parse("md5").expect("md5"))
        .remote_options(["--log-file=/l g*"])
        .build();
    let argv = lossy(
        RemoteInvocationBuilder::new(&config, RemoteRole::Receiver).build(OsStr::new("src dir")),
    );
    assert_has_slices(
        &argv,
        &[
            &["--checksum-choice=md5"],
            &["--skip-compress=gz/j\\*g"],
            &["--files-from", "/f\\ rom~"],
            &["--log-file=/l\\ g\\*"],
            &[".", "src\\ dir"],
        ],
    );
}

/// Under `--secluded-args` the values travel NUL-separated on stdin, which no
/// shell re-splits, so upstream skips the escaping but keeps the split forms.
#[test]
fn secluded_args_ship_values_verbatim_like_upstream() {
    let config = ClientConfig::builder()
        .partial_directory(Some(".p d\"q"))
        .temp_directory(Some("/t'm p"))
        .remote_options(["--log-file=/l g*"])
        .protect_args(Some(true))
        .build();
    let secluded = RemoteInvocationBuilder::new(&config, RemoteRole::Sender)
        .build_secluded(&[OsStr::new("dst")]);
    assert_has_slices(
        &lossy(secluded.stdin_args),
        &[
            &["--partial-dir", ".p d\"q"],
            &["--temp-dir", "/t'm p"],
            &["--log-file=/l g*"],
        ],
    );
}
