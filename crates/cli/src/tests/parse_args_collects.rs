use super::common::*;
use super::*;

#[test]
fn parse_args_collects_chmod_values() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--chmod=Du+rwx"),
        OsString::from("--chmod"),
        OsString::from("Fgo-w"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.chmod,
        vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")]
    );
}

#[test]
fn parse_args_collects_link_dest_paths() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--link-dest=baseline"),
        OsString::from("--link-dest=/var/cache"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.link_dests,
        vec![PathBuf::from("baseline"), PathBuf::from("/var/cache")]
    );
}

#[test]
fn parse_args_collects_out_format_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%f %b"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.out_format, Some(OsString::from("%f %b")));
}

#[test]
fn parse_args_collects_filter_patterns() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*.tmp"),
        OsString::from("--include"),
        OsString::from("important/**"),
        OsString::from("--filter"),
        OsString::from("+ staging/**"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.excludes, vec![OsString::from("*.tmp")]);
    assert_eq!(parsed.includes, vec![OsString::from("important/**")]);
    assert_eq!(parsed.filters, vec![OsString::from("+ staging/**")]);
}

#[test]
fn parse_args_collects_filter_files() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("excludes.txt"),
        OsString::from("--include-from"),
        OsString::from("includes.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.exclude_from, vec![OsString::from("excludes.txt")]);
    assert_eq!(parsed.include_from, vec![OsString::from("includes.txt")]);
}

#[test]
fn parse_args_collects_reference_destinations() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compare-dest"),
        OsString::from("compare"),
        OsString::from("--copy-dest"),
        OsString::from("copy"),
        OsString::from("--link-dest"),
        OsString::from("link"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compare_destinations, vec![OsString::from("compare")]);
    assert_eq!(parsed.copy_destinations, vec![OsString::from("copy")]);
    assert_eq!(parsed.link_destinations, vec![OsString::from("link")]);
}

#[test]
fn parse_args_collects_files_from_paths() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--files-from"),
        OsString::from("list-a"),
        OsString::from("--files-from"),
        OsString::from("list-b"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.files_from,
        vec![OsString::from("list-a"), OsString::from("list-b")]
    );
}

#[test]
fn parse_args_collects_protocol_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=30"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.protocol, Some(OsString::from("30")));
}

#[test]
fn parse_args_collects_timeout_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--timeout=90"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.timeout, Some(OsString::from("90")));
}

#[test]
fn parse_args_collects_contimeout_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--contimeout=45"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.contimeout, Some(OsString::from("45")));
}

#[test]
fn parse_args_collects_max_delete_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-delete=12"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.max_delete, Some(OsString::from("12")));
}

#[test]
fn parse_args_collects_checksum_seed_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-seed=42"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.checksum_seed, Some(42));
}

#[test]
fn parse_args_collects_min_max_size_values() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--min-size=1.5K"),
        OsString::from("--max-size=2M"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.min_size, Some(OsString::from("1.5K")));
    assert_eq!(parsed.max_size, Some(OsString::from("2M")));
}
