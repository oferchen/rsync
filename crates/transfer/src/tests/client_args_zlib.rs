//! Daemon-mode `-z` detection over the client's verbatim argv.
//!
//! upstream: options.c:2898-2904 `server_options()` appends `z` to `argstr`
//! for CPRES_ZLIB and then `maybe_add_e_option()` (options.c:3202) appends
//! `e` plus the capability letters last. Popt binds everything after `-e`
//! (`{"rsh", 'e', POPT_ARG_STRING, ...}`, options.c:837) as its value, so a
//! `z` among the capability letters is never the compress option. Reading it
//! as `-z` would turn compression on for a client that never asked for it,
//! and the two sides would then disagree on the token stream format.

use crate::client_args_request_zlib;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn z_inside_the_e_value_is_not_compression() {
    assert!(!client_args_request_zlib(&args(&[
        "--server",
        "--sender",
        "-logDtpre.iLsfxCIvuz",
        ".",
        "mod/",
    ])));
}

#[test]
fn z_inside_a_standalone_e_cluster_is_not_compression() {
    assert!(!client_args_request_zlib(&args(&[
        "--server",
        "-e.LsfxCIvuz",
        ".",
        "mod/",
    ])));
}

#[test]
fn z_in_the_flag_cluster_before_e_is_compression() {
    assert!(client_args_request_zlib(&args(&[
        "--server",
        "--sender",
        "-vlogDtprze.iLsfxCIvu",
        ".",
        "mod/",
    ])));
}

#[test]
fn z_in_a_protocol_29_cluster_without_e_is_compression() {
    // upstream: options.c:3209 - maybe_add_e_option() adds nothing below
    // protocol 30, so the whole cluster is option letters.
    assert!(client_args_request_zlib(&args(&[
        "--server",
        "-logDtprz",
        ".",
        "mod/"
    ])));
}

#[test]
fn long_options_are_not_scanned_for_z() {
    assert!(!client_args_request_zlib(&args(&[
        "--server",
        "--compress-level=9",
        "--zl=1",
        "-logDtpre.iLsfxCIvu",
        ".",
        "mod/",
    ])));
}
