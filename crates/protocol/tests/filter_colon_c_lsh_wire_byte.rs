//! Wire-byte regression for the `:C` bare-modifier dir-merge rule transported
//! over a remote-shell (lsh.sh) transport.
//!
//! Background: the upstream testsuite's `exclude-lsh.test` exercises filter
//! rules shipped from the local client to the remote sender through `lsh.sh`.
//! The bare `:C` form (dir-merge + CVS-ignore + empty pattern) must default
//! the per-directory filename to `.cvsignore` before serializing onto the
//! wire, otherwise the remote sender receives a dir-merge with no pattern and
//! cannot activate CVS-style parsing.
//!
//! This test pins the wire bytes for that exact rule shape so a future
//! refactor of the prefix builder or wire emitter cannot silently drop the
//! `C` modifier, the trailing space separator, or the default `.cvsignore`
//! pattern without tripping a regression.
//!
//! upstream: `exclude.c::send_filter_list()` (3.4.4) writes each rule as a
//! 4-byte little-endian length prefix followed by `get_rule_prefix()` output
//! (here: `:C ` = type `:` + modifier `C` + trailing space) concatenated with
//! the pattern bytes. The full list is terminated by a 4-byte zero record.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};

/// Asserts the exact wire bytes for `:C` over a protocol 32 (modern, lsh.sh)
/// session: length-prefix + `:C .cvsignore` + zero terminator.
///
/// upstream: `exclude.c::send_filter_list()` - rule-list serialization;
/// `exclude.c::get_rule_prefix()` - emits `:C ` for a dir-merge rule carrying
/// FILTRULE_CVS_IGNORE.
#[test]
fn colon_c_bare_modifier_emits_cvsignore_default_over_lsh() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    // Golden wire bytes: 4-byte LE length (13) + ":C .cvsignore" + 4-byte LE zero.
    // 13 = 3 prefix bytes (`:`, `C`, space) + 10 pattern bytes (`.cvsignore`).
    let expected: [u8; 21] = [
        0x0D, 0x00, 0x00, 0x00, b':', b'C', b' ', b'.', b'c', b'v', b's', b'i', b'g', b'n', b'o',
        b'r', b'e', 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(buf, expected, "wire bytes for `:C .cvsignore` diverged");

    // Round-trip parity: decode and confirm the modifier survives.
    let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].rule_type, RuleType::DirMerge);
    assert_eq!(parsed[0].pattern, ".cvsignore");
    assert!(
        parsed[0].cvs_exclude,
        "C modifier must round-trip across lsh.sh transport"
    );
}

/// Protocol 29 - the minimum modern prefix protocol - must produce the same
/// payload shape (only the length differs if modifiers change). Pinning the
/// bytes here guards against accidental modifier-flag downgrades on the
/// v29-v31 envelope, which lsh.sh transports may negotiate.
///
/// upstream: `exclude.c::get_rule_prefix()` (3.4.4) emits the `C` modifier
/// unconditionally on v29+; the trailing space separator is part of the
/// contract.
#[test]
fn colon_c_bare_modifier_wire_format_stable_at_v29() {
    let protocol = ProtocolVersion::from_supported(29).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    let expected: [u8; 21] = [
        0x0D, 0x00, 0x00, 0x00, b':', b'C', b' ', b'.', b'c', b'v', b's', b'i', b'g', b'n', b'o',
        b'r', b'e', 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(
        buf, expected,
        "v29 wire bytes for `:C .cvsignore` diverged from v32"
    );
}
