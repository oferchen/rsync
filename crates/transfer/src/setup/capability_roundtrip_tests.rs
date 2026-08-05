//! Round-trip guard for the `-e.<caps>` capability string: the letters the
//! client advertises via `build_capability_string[_suffix]`, when parsed by the
//! server-side capability decoder (`parse_client_info` ->
//! `build_compat_flags_from_client_info`), must decode to exactly the
//! capability set that was advertised. Emit and decode of the capability string
//! can never silently diverge - a dropped or renamed letter on either side
//! fails here.
//!
//! # Upstream Reference
//!
//! - `options.c:3021-3068 maybe_add_e_option()` - the client emits the
//!   `-e.<letters>` capability string.
//! - `compat.c:712-734` - the server reads `client_info` and sets the matching
//!   `CF_*` flags. The letter meanings: `i` INC_RECURSE, `L` SYMLINK_TIMES,
//!   `s` SYMLINK_ICONV, `f` SAFE_FILE_LIST, `x` AVOID_XATTR_OPTIMIZATION,
//!   `C` CHECKSUM_SEED_FIX, `I` INPLACE_PARTIAL_DIR, `v` VARINT_FLIST_FLAGS,
//!   `u` ID0_NAMES.
//!
//! # Protocol dimension
//!
//! The oc capability letter set is invariant across protocol 28-32: the
//! `CF_*` capabilities are all protocol-30+ concepts, and their letters do
//! not change between 30, 31 and 32. The only protocol-driven variation is
//! whether `allow_inc_recurse` is set - INC_RECURSE (`i`) requires protocol
//! 30+, so a protocol 28/29 negotiation passes `allow_inc_recurse = false`
//! and omits `i`, while protocol 30+ may set it. These tests therefore
//! parametrize on `allow_inc_recurse` (the sole per-protocol difference) and
//! assert the round-trip holds in both states. Role (sender/receiver) does not
//! affect the advertised letters or their decode, so it is not a parameter.

use super::{
    CAPABILITY_MAPPINGS, CompatibilityFlags, build_capability_string_suffix,
    build_compat_flags_from_client_info, parse_client_info,
};

/// Emits the capability suffix for `allow_inc_recurse`, embeds it in a realistic
/// compact server flag token (as an SSH server receives it), runs the same
/// parse the server uses, and returns `(emitted_suffix, decoded_flags)`.
fn emit_then_decode(allow_inc_recurse: bool) -> (String, CompatibilityFlags) {
    let suffix = build_capability_string_suffix(allow_inc_recurse);
    // The server receives the caps as the tail of the compact flag string, e.g.
    // `-logDtpre.iLsfxCIvu` (upstream options.c:2728 appends into the same argstr).
    let compact = format!("-logDtpr{suffix}");
    let client_info = parse_client_info(std::slice::from_ref(&compact)).into_owned();
    let flags = build_compat_flags_from_client_info(&client_info, allow_inc_recurse);
    (suffix, flags)
}

/// Core round-trip, table-driven against the single source of truth
/// (`CAPABILITY_MAPPINGS`): for every capability, "the letter was advertised"
/// must equal "the flag was decoded". Any emit/parse asymmetry - a letter
/// emitted but not decoded, or decoded but not emitted - fails here.
fn assert_letters_and_flags_agree(allow_inc_recurse: bool) {
    let (suffix, flags) = emit_then_decode(allow_inc_recurse);

    for mapping in CAPABILITY_MAPPINGS {
        let advertised = suffix.contains(mapping.char);
        let decoded = flags.contains(mapping.flag);
        assert_eq!(
            advertised, decoded,
            "capability '{}' must round-trip (advertised={advertised}, decoded={decoded}) \
             for allow_inc_recurse={allow_inc_recurse}; suffix={suffix:?}",
            mapping.char,
        );
    }

    // No bit outside the advertised set may appear in the decoded flags: the
    // decoded set is exactly the union of the per-letter mappings.
    let expected: CompatibilityFlags = CAPABILITY_MAPPINGS
        .iter()
        .filter(|m| suffix.contains(m.char))
        .fold(CompatibilityFlags::from_bits(0), |acc, m| acc | m.flag);
    assert_eq!(
        flags, expected,
        "decoded flags must equal exactly the advertised capability set: suffix={suffix:?}"
    );
}

/// Protocol < 30 (`allow_inc_recurse = false`): every advertised capability
/// letter round-trips to its flag, and INC_RECURSE is neither advertised nor
/// decoded (no `i`).
#[test]
fn capability_string_round_trips_without_inc_recurse() {
    assert_letters_and_flags_agree(false);

    let (suffix, flags) = emit_then_decode(false);
    assert!(
        !suffix.contains('i'),
        "protocol < 30 must not advertise inc-recurse: {suffix:?}"
    );
    assert!(
        !flags.contains(CompatibilityFlags::INC_RECURSE),
        "inc-recurse must not decode when not advertised: {suffix:?}"
    );
}

/// Protocol >= 30 (`allow_inc_recurse = true`): the same round-trip holds and
/// INC_RECURSE is both advertised (`i`) and decoded.
#[test]
fn capability_string_round_trips_with_inc_recurse() {
    assert_letters_and_flags_agree(true);

    let (suffix, flags) = emit_then_decode(true);
    assert!(
        suffix.contains('i'),
        "protocol >= 30 with inc-recurse must advertise `i`: {suffix:?}"
    );
    assert!(
        flags.contains(CompatibilityFlags::INC_RECURSE),
        "inc-recurse must decode when advertised: {suffix:?}"
    );
}

/// The stable, always-advertised capabilities (independent of the inc-recurse
/// state) each round-trip to their flag - a readable pin on the common set so a
/// regression naming a specific letter is self-documenting.
#[test]
fn stable_capabilities_round_trip_to_their_flags() {
    for allow_inc_recurse in [false, true] {
        let (suffix, flags) = emit_then_decode(allow_inc_recurse);
        // These do not depend on protocol / inc-recurse and are advertised on
        // this (Unix, all-features) build.
        for (ch, flag) in [
            ('f', CompatibilityFlags::SAFE_FILE_LIST),
            ('x', CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
            ('C', CompatibilityFlags::CHECKSUM_SEED_FIX),
            ('I', CompatibilityFlags::INPLACE_PARTIAL_DIR),
            ('v', CompatibilityFlags::VARINT_FLIST_FLAGS),
            ('u', CompatibilityFlags::ID0_NAMES),
        ] {
            assert!(suffix.contains(ch), "`{ch}` must be advertised: {suffix:?}");
            assert!(
                flags.contains(flag),
                "`{ch}` must decode to its flag: {suffix:?}"
            );
        }
    }
}

/// The `parse_client_info` scan tolerates the exact compact-token shapes an SSH
/// server sees, and each still round-trips: the leading `VER.SUB`/`.` placeholder
/// upstream inserts (`-e.<caps>` for a release peer, `-e<ver>.<sub><caps>` for a
/// pre-release peer) must not leak into the capability letters or change the
/// decoded flag set.
#[test]
fn capability_payload_shapes_round_trip() {
    let suffix = build_capability_string_suffix(true);
    let letters = suffix.strip_prefix("e.").expect("suffix starts with e.");

    // Release form embedded in a compact flag string.
    let release = format!("-logDtpr{suffix}");
    // Pre-release form with an explicit VER.SUB before the letters.
    let pre_release = format!("-logDtpre32.7{letters}");

    let release_info = parse_client_info(std::slice::from_ref(&release)).into_owned();
    let pre_info = parse_client_info(std::slice::from_ref(&pre_release)).into_owned();

    // The release placeholder `.` is stripped; the pre-release `32.7` prefix is
    // part of client_info (upstream keeps it - it is consumed by the subprotocol
    // parse, not the capability parse), but the capability LETTERS are present in
    // both, so the decoded flag set is identical.
    assert!(
        release_info.contains('v') && pre_info.contains('v'),
        "both payload shapes must carry the capability letters: {release_info:?} / {pre_info:?}"
    );
    let release_flags = build_compat_flags_from_client_info(&release_info, true);
    let pre_flags = build_compat_flags_from_client_info(&pre_info, true);
    assert_eq!(
        release_flags, pre_flags,
        "the VER.SUB placeholder must not change the decoded capability set"
    );
}
