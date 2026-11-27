//! A server reads the long options a 3.5.2 client forwards with `-M`.
//!
//! upstream: options.c:3251-3258 appends each `--remote-option` after the
//! other server arguments, and the server parses them with the same
//! long_options[] table as the client (options.c:610-876, :1515).

use std::ffi::OsString;

use super::flags::{
    ArgvRefusal, normalize_server_argv, parse_server_long_flags, refused_server_option,
};
use super::parse::parse_server_flag_string_and_args;

/// A pushed server argv with `options` where `-M` puts them, normalised as
/// the server normalises it.
fn server_argv(options: &[&str]) -> Vec<OsString> {
    let argv: Vec<OsString> = ["oc-rsync", "--server", "-logDtpre.iLsfxCIvu"]
        .into_iter()
        .chain(options.iter().copied())
        .chain([".", "dst/"])
        .map(OsString::from)
        .collect();
    let mut normalized = normalize_server_argv(&argv).expect("normalized");
    normalized.remove(0);
    normalized
}

/// Every option a 3.5.2 server accepted from a 3.5.2 client's `-M` on the
/// host where oc's server refused it as `unknown option` (push and pull,
/// remote shell). None of them may be refused, and none may leak into the
/// operands as a destination.
const ACCEPTED_BY_3_5_2: &[&str] = &[
    "--verbose",
    "--no-verbose",
    "--no-v",
    "--stderr=all",
    "--quiet",
    "--motd",
    "--no-motd",
    "--human-readable",
    "--no-human-readable",
    "--no-h",
    "--archive",
    "--recursive",
    "--no-recursive",
    "--inc-recursive",
    "--no-inc-recursive",
    "--i-r",
    "--no-i-r",
    "--dirs",
    "--no-dirs",
    "--no-d",
    "--perms",
    "--no-perms",
    "--no-p",
    "--executability",
    "--no-acls",
    "--no-A",
    "--no-xattrs",
    "--no-X",
    "--times",
    "--no-times",
    "--no-t",
    "--no-atimes",
    "--no-U",
    "--no-open-noatime",
    "--no-crtimes",
    "--no-N",
    "--omit-dir-times",
    "--no-omit-dir-times",
    "--no-O",
    "--omit-link-times",
    "--no-omit-link-times",
    "--no-J",
    "--no-super",
    "--owner",
    "--no-owner",
    "--no-o",
    "--group",
    "--no-D",
    "--devices",
    "--no-devices",
    "--no-write-devices",
    "--links",
    "--no-links",
    "--no-l",
    "--copy-links",
    "--munge-links",
    "--no-munge-links",
    "--copy-dirlinks",
    "--keep-dirlinks",
    "--hard-links",
    "--no-hard-links",
    "--no-H",
    "--implied-dirs",
    "--i-d",
    "--no-i-d",
    "--chmod=u+w",
    "--ignore-times",
    "--one-file-system",
    "--no-one-file-system",
    "--no-x",
    "--update",
    "--sparse",
    "--no-sparse",
    "--no-S",
    "--no-inplace",
    "--append-verify",
    "--no-append",
    "--del",
    "--no-force",
    "--no-ignore-errors",
    "--cvs-exclude",
    "--whole-file",
    "--no-whole-file",
    "--no-checksum",
    "--no-c",
    "--block-size=1024",
    "--fuzzy",
    "--no-fuzzy",
    "--no-y",
    "--no-compress",
    "--no-z",
    "--compress-threads=1",
    "--zt=1",
    "--progress",
    "--no-progress",
    "--no-partial",
    "--no-delay-updates",
    "--no-delay-symlinks",
    "--prune-empty-dirs",
    "--no-prune-empty-dirs",
    "--no-m",
    "--log-file-format=%o:%n",
    "--out-format=%n",
    "--itemize-changes",
    "--no-itemize-changes",
    "--no-i",
    "--no-bwlimit",
    "--no-backup",
    "--no-from0",
    "--old-args",
    "--no-old-args",
    "--no-secluded-args",
    "--no-protect-args",
    "--no-s",
    "--no-numeric-ids",
    "--no-timeout",
    "--contimeout=5",
    "--no-contimeout",
    "--time-limit=10",
    "--rsh=ssh",
    "--rsync-path=rsync",
    "--no-iconv",
    "--ipv4",
    "--ipv6",
    "--8-bit-output",
    "--no-8-bit-output",
    "--no-8",
    "--address=127.0.0.1",
    "--port=873",
    "--sockopts=SO_KEEPALIVE",
    "--password-file=/dev/null",
    "--early-input=/dev/null",
    "--blocking-io",
    "--no-blocking-io",
    "--outbuf=L",
    "--chown=u:g",
];

#[test]
fn every_option_3_5_2_accepts_from_the_client_is_accepted() {
    for &option in ACCEPTED_BY_3_5_2 {
        let args = server_argv(&[option]);
        assert_eq!(refused_server_option(&args), None, "{option}");
        let (_, operands) = parse_server_flag_string_and_args(&args);
        assert_eq!(operands, [OsString::from("dst/")], "{option}");
    }
}

/// Options 3.5.2's server refuses keep their popt and rsync wording.
#[test]
fn refusals_keep_3_5_2_wording() {
    let refusal = |option: &str| refused_server_option(&server_argv(&[option]));
    let popt = |text: &str| {
        Some(ArgvRefusal {
            text: format!("on remote machine: {text}"),
            after_server: true,
        })
    };
    let own = |text: &str| {
        Some(ArgvRefusal {
            text: text.to_owned(),
            after_server: true,
        })
    };
    assert_eq!(
        refusal("--verbose=1"),
        popt("--verbose=1: option does not take an argument")
    );
    assert_eq!(
        refusal("--delete=x"),
        popt("--delete=x: option does not take an argument")
    );
    assert_eq!(refusal("--bogus"), popt("--bogus: unknown option"));
    assert_eq!(
        refusal("--write-batch=b"),
        popt("--write-batch=b: unknown option")
    );
    assert_eq!(
        refusal("--stderr=bogus"),
        own("--stderr mode \"bogus\" is not one of errors, all, or client")
    );
    assert_eq!(
        refusal("--chmod=q+z"),
        own("Invalid argument passed to --chmod (q+z)")
    );
    assert_eq!(refusal("--stderr=e"), None);
}

/// Each forwarded negation or alias reaches the field its upstream variable
/// maps to, and a later option overrides an earlier one as popt's pass does.
#[test]
fn forwarded_long_options_set_the_server_fields() {
    let flags = |options: &[&str]| parse_server_long_flags(&server_argv(options));
    assert!(!flags(&["--inplace", "--no-inplace"]).inplace);
    let append = flags(&["--append-verify"]);
    assert!(append.append && append.append_verify);
    let append = flags(&["--append-verify", "--no-append"]);
    assert!(!append.append && !append.append_verify);
    assert!(!flags(&["--partial", "--no-partial"]).partial);
    assert!(!flags(&["--force", "--no-force"]).force);
    assert!(!flags(&["--ignore-errors", "--no-ignore-errors"]).ignore_errors);
    assert!(!flags(&["--numeric-ids", "--no-numeric-ids"]).numeric_ids);
    assert!(!flags(&["--from0", "--no-from0"]).from0);
    assert!(!flags(&["--write-devices", "--no-write-devices"]).write_devices);
    assert!(!flags(&["--open-noatime", "--no-open-noatime"]).open_noatime);
    assert!(!flags(&["--delay-updates", "--no-delay-updates"]).delay_updates);
    assert!(!flags(&["--delay-symlinks", "--no-delay-symlinks"]).delay_symlinks);
    assert!(flags(&["--del"]).delete);
    assert!(flags(&["--no-recursive"]).no_recurse);
    assert!(flags(&["--no-whole-file"]).no_whole_file);
    assert!(flags(&["--no-i-d"]).no_implied_dirs);
    assert!(!flags(&["--no-implied-dirs", "--i-d"]).no_implied_dirs);
    assert!(flags(&["--no-i-r"]).no_inc_recurse);
    assert!(!flags(&["--no-inc-recursive", "--inc-recursive"]).no_inc_recurse);
    assert_eq!(flags(&["--devices"]).devices, Some(true));
    assert_eq!(flags(&["--no-devices"]).devices, Some(false));
    assert_eq!(flags(&["--munge-links"]).munge_symlinks, Some(true));
    assert_eq!(flags(&["--no-munge-links"]).munge_symlinks, Some(false));
    assert_eq!(
        flags(&["--fake-super", "--no-super"]).fake_super,
        Some(false)
    );
    assert_eq!(flags(&["--fake-super", "--super"]).fake_super, Some(false));
    assert_eq!(flags(&["--iconv=utf8", "--no-iconv"]).iconv, None);
    assert_eq!(flags(&["--bwlimit=10", "--no-bwlimit"]).bwlimit, None);
    assert_eq!(flags(&["--timeout=10", "--no-timeout"]).timeout, None);
    assert_eq!(
        flags(&["--chmod=u+w", "--chmod", "go-r"]).chmod,
        ["u+w", "go-r"]
    );
    assert_eq!(flags(&["--zt=2"]).compress_threads.as_deref(), Some("2"));
    assert_eq!(
        flags(&["--compress-threads=3"]).compress_threads.as_deref(),
        Some("3")
    );
    assert_eq!(
        flags(&["--out-format=%n"]).log_format.as_deref(),
        Some("%n")
    );
    assert_eq!(flags(&["--time-limit=5"]).stop_after.as_deref(), Some("5"));
    assert_eq!(flags(&["--cc=md5"]).checksum_choice.as_deref(), Some("md5"));
    assert_eq!(flags(&["--zl=3"]).compression_level.as_deref(), Some("3"));
    assert_eq!(
        flags(&["--stderr=all"]).msgs2stderr,
        Some(core::server::writer::MessageRoute::AllToStderr)
    );
    assert_eq!(flags(&["--chown=u:g"]).usermap.as_deref(), Some("*:u"));
}

/// A long switch lands in the compact flag string the transfer reads.
#[test]
fn long_switches_reach_the_flag_string() {
    let (flag_string, _) =
        parse_server_flag_string_and_args(&server_argv(&["--no-p", "--hard-links"]));
    assert_eq!(flag_string, "-logDtrHe.iLsfxCIvu");
}
