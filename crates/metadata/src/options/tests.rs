use super::*;
use crate::chmod::ChmodModifiers;

#[test]
fn attrs_flags_constants_match_upstream() {
    // upstream rsync.h:192-196
    assert_eq!(AttrsFlags::REPORT.bits(), 1 << 0);
    assert_eq!(AttrsFlags::SKIP_MTIME.bits(), 1 << 1);
    assert_eq!(AttrsFlags::ACCURATE_TIME.bits(), 1 << 2);
    assert_eq!(AttrsFlags::SKIP_ATIME.bits(), 1 << 3);
    assert_eq!(AttrsFlags::SKIP_CRTIME.bits(), 1 << 5);
}

#[test]
fn attrs_flags_skip_all_times_combines_three_skip_flags() {
    let combined = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
    assert_eq!(AttrsFlags::SKIP_ALL_TIMES, combined);
    assert!(AttrsFlags::SKIP_ALL_TIMES.skip_mtime());
    assert!(AttrsFlags::SKIP_ALL_TIMES.skip_atime());
    assert!(AttrsFlags::SKIP_ALL_TIMES.skip_crtime());
    assert!(!AttrsFlags::SKIP_ALL_TIMES.accurate_time());
    assert!(!AttrsFlags::SKIP_ALL_TIMES.report());
}

#[test]
fn attrs_flags_empty_has_no_bits_set() {
    let flags = AttrsFlags::empty();
    assert!(flags.is_empty());
    assert_eq!(flags.bits(), 0);
    assert!(!flags.skip_mtime());
    assert!(!flags.skip_atime());
    assert!(!flags.skip_crtime());
    assert!(!flags.accurate_time());
    assert!(!flags.report());
}

#[test]
fn attrs_flags_default_is_empty() {
    assert_eq!(AttrsFlags::default(), AttrsFlags::empty());
}

#[test]
fn attrs_flags_from_bits_round_trips() {
    let flags = AttrsFlags::SKIP_MTIME | AttrsFlags::ACCURATE_TIME;
    let raw = flags.bits();
    assert_eq!(AttrsFlags::from_bits(raw), flags);
}

#[test]
fn attrs_flags_contains_checks_subset() {
    let all = AttrsFlags::SKIP_ALL_TIMES;
    assert!(all.contains(AttrsFlags::SKIP_MTIME));
    assert!(all.contains(AttrsFlags::SKIP_ATIME));
    assert!(all.contains(AttrsFlags::SKIP_CRTIME));
    assert!(!all.contains(AttrsFlags::REPORT));
    assert!(!all.contains(AttrsFlags::ACCURATE_TIME));
}

#[test]
fn attrs_flags_union_combines_flags() {
    let a = AttrsFlags::REPORT;
    let b = AttrsFlags::SKIP_MTIME;
    let combined = a.union(b);
    assert!(combined.report());
    assert!(combined.skip_mtime());
    assert!(!combined.skip_atime());
}

#[test]
fn attrs_flags_bitor_assign() {
    let mut flags = AttrsFlags::empty();
    flags |= AttrsFlags::SKIP_MTIME;
    flags |= AttrsFlags::SKIP_ATIME;
    assert!(flags.skip_mtime());
    assert!(flags.skip_atime());
    assert!(!flags.skip_crtime());
}

#[test]
fn attrs_flags_bitand_masks() {
    let flags = AttrsFlags::SKIP_ALL_TIMES | AttrsFlags::REPORT;
    let masked = flags & AttrsFlags::SKIP_MTIME;
    assert!(masked.skip_mtime());
    assert!(!masked.skip_atime());
    assert!(!masked.report());
}

#[test]
fn attrs_flags_bit4_is_unused_gap() {
    // upstream rsync.h has a gap between bit 3 (SKIP_ATIME) and bit 5
    // (SKIP_CRTIME). Bit 4 is unused, matching upstream layout.
    assert_eq!(AttrsFlags::SKIP_ATIME.bits(), 0x08);
    assert_eq!(AttrsFlags::SKIP_CRTIME.bits(), 0x20);
    // No constant occupies bit 4 (0x10)
}

#[test]
fn attrs_flags_individual_predicate_methods() {
    assert!(AttrsFlags::REPORT.report());
    assert!(!AttrsFlags::REPORT.skip_mtime());

    assert!(AttrsFlags::SKIP_MTIME.skip_mtime());
    assert!(!AttrsFlags::SKIP_MTIME.skip_atime());

    assert!(AttrsFlags::ACCURATE_TIME.accurate_time());
    assert!(!AttrsFlags::ACCURATE_TIME.skip_mtime());

    assert!(AttrsFlags::SKIP_ATIME.skip_atime());
    assert!(!AttrsFlags::SKIP_ATIME.skip_crtime());

    assert!(AttrsFlags::SKIP_CRTIME.skip_crtime());
    assert!(!AttrsFlags::SKIP_CRTIME.skip_mtime());
}

#[test]
fn attrs_flags_upstream_set_file_attrs_scenario() {
    // Mirrors upstream rsync.c:583-593 logic for omit_dir_times / omit_link_times
    // When dir/link times are omitted, all three time-skip flags are set.
    let omit_times = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
    assert_eq!(omit_times, AttrsFlags::SKIP_ALL_TIMES);

    // When only mtime is not preserved, only SKIP_MTIME is set.
    let no_preserve_mtime = AttrsFlags::SKIP_MTIME;
    assert!(no_preserve_mtime.skip_mtime());
    assert!(!no_preserve_mtime.skip_atime());
    assert!(!no_preserve_mtime.skip_crtime());
}

#[test]
fn attrs_flags_upstream_accurate_time_with_report() {
    // Mirrors upstream generator.c:1827 where quick-check match combines
    // maybe_ATTRS_REPORT and maybe_ATTRS_ACCURATE_TIME.
    let flags = AttrsFlags::REPORT | AttrsFlags::ACCURATE_TIME;
    assert!(flags.report());
    assert!(flags.accurate_time());
    assert!(!flags.skip_mtime());
}

#[test]
fn attrs_flags_upstream_receiver_skip_all_when_not_ok_to_set_time() {
    // Mirrors upstream rsync.c:749 and :774 where ok_to_set_time is false:
    // ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME
    let flags = AttrsFlags::SKIP_MTIME | AttrsFlags::SKIP_ATIME | AttrsFlags::SKIP_CRTIME;
    assert!(flags.skip_mtime());
    assert!(flags.skip_atime());
    assert!(flags.skip_crtime());
    assert!(!flags.accurate_time());
}

#[test]
fn defaults_match_expected_configuration() {
    let options = MetadataOptions::new();

    assert!(!options.owner());
    assert!(!options.group());
    assert!(!options.executability());
    assert!(options.permissions());
    assert!(options.times());
    assert!(!options.atimes());
    assert!(!options.crtimes());
    assert!(!options.numeric_ids_enabled());
    assert!(!options.fake_super_enabled());
    assert!(options.owner_override().is_none());
    assert!(options.group_override().is_none());
    assert!(options.chmod().is_none());
    assert!(options.user_mapping().is_none());
    assert!(options.group_mapping().is_none());

    assert!(!options.destination_is_new());

    assert_eq!(MetadataOptions::default(), options);
}

#[cfg(unix)]
#[test]
fn builder_methods_apply_requested_flags() {
    let modifiers = ChmodModifiers::parse("u=rw").expect("parse modifiers");

    let user_map = UserMapping::parse("1000:2000").expect("parse usermap");
    let group_map = GroupMapping::parse("*:3000").expect("parse groupmap");

    let options = MetadataOptions::new()
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_executability(true)
        .preserve_permissions(false)
        .preserve_times(false)
        .numeric_ids(true)
        .with_owner_override(Some(42))
        .with_group_override(Some(7))
        .with_chmod(Some(modifiers.clone()))
        .with_user_mapping(Some(user_map.clone()))
        .with_group_mapping(Some(group_map.clone()));

    assert!(options.owner());
    assert!(options.group());
    assert!(options.executability());
    assert!(!options.permissions());
    assert!(!options.times());
    assert!(options.numeric_ids_enabled());
    assert_eq!(options.owner_override(), Some(42));
    assert_eq!(options.group_override(), Some(7));
    assert_eq!(options.chmod(), Some(&modifiers));
    assert_eq!(options.user_mapping(), Some(&user_map));
    assert_eq!(options.group_mapping(), Some(&group_map));
}

#[test]
fn destination_is_new_defaults_to_false_and_can_be_set() {
    let options = MetadataOptions::new();
    assert!(!options.destination_is_new());

    let options = options.with_destination_is_new(true);
    assert!(options.destination_is_new());

    let options = options.with_destination_is_new(false);
    assert!(!options.destination_is_new());
}

#[test]
fn keep_dirlinks_defaults_to_false_and_can_be_set() {
    let options = MetadataOptions::new();
    assert!(!options.keep_dirlinks());

    let options = options.with_keep_dirlinks(true);
    assert!(options.keep_dirlinks());

    let options = options.with_keep_dirlinks(false);
    assert!(!options.keep_dirlinks());
}

#[test]
fn fake_super_can_be_enabled() {
    let options = MetadataOptions::new().fake_super(true);
    assert!(options.fake_super_enabled());

    let disabled = MetadataOptions::new().fake_super(false);
    assert!(!disabled.fake_super_enabled());
}

#[test]
fn has_any_preservation_true_for_defaults() {
    // Default MetadataOptions preserves permissions and times.
    let opts = MetadataOptions::new();
    assert!(
        opts.has_any_preservation(),
        "defaults have permissions + times active"
    );
}

#[test]
fn has_any_preservation_false_when_all_cleared() {
    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false);
    assert!(!opts.has_any_preservation(), "no preservation flags active");
}

#[test]
fn has_any_preservation_detects_each_flag() {
    let base = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false);
    assert!(!base.has_any_preservation());

    assert!(base.clone().preserve_owner(true).has_any_preservation());
    assert!(base.clone().preserve_group(true).has_any_preservation());
    assert!(
        base.clone()
            .preserve_executability(true)
            .has_any_preservation()
    );
    assert!(base.clone().preserve_times(true).has_any_preservation());
    assert!(base.clone().preserve_atimes(true).has_any_preservation());
    assert!(base.clone().preserve_crtimes(true).has_any_preservation());
    assert!(base.clone().fake_super(true).has_any_preservation());
    assert!(
        base.clone()
            .with_owner_override(Some(0))
            .has_any_preservation()
    );
    assert!(
        base.clone()
            .with_group_override(Some(0))
            .has_any_preservation()
    );
    assert!(
        base.clone()
            .with_chmod(Some(ChmodModifiers::parse("g+x").expect("parse")))
            .has_any_preservation()
    );
}

#[test]
fn overrides_and_chmod_can_be_cleared() {
    let base = MetadataOptions::new()
        .with_owner_override(Some(13))
        .with_group_override(Some(24))
        .with_chmod(Some(ChmodModifiers::parse("g+x").expect("parse modifiers")));

    let cleared = base
        .with_owner_override(None)
        .with_group_override(None)
        .with_chmod(None)
        .preserve_owner(false)
        .preserve_group(false)
        .preserve_permissions(true)
        .preserve_times(true)
        .numeric_ids(false);

    assert!(!cleared.owner());
    assert!(!cleared.group());
    assert!(cleared.permissions());
    assert!(cleared.times());
    assert!(!cleared.numeric_ids_enabled());
    assert!(cleared.owner_override().is_none());
    assert!(cleared.group_override().is_none());
    assert!(cleared.chmod().is_none());
}

#[test]
fn requires_apply_true_for_default_options() {
    // Default MetadataOptions has preserve_permissions=true and
    // preserve_times=true, so requires_apply must be true.
    let opts = MetadataOptions::new();
    assert!(opts.requires_apply());
}

#[test]
fn requires_apply_false_when_all_flags_cleared() {
    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false)
        .preserve_owner(false)
        .preserve_group(false);
    assert!(!opts.requires_apply());
}

#[test]
fn requires_apply_true_for_each_flag_independently() {
    let base = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false);

    assert!(base.clone().preserve_owner(true).requires_apply());
    assert!(base.clone().preserve_group(true).requires_apply());
    assert!(base.clone().preserve_executability(true).requires_apply());
    assert!(base.clone().preserve_permissions(true).requires_apply());
    assert!(base.clone().preserve_times(true).requires_apply());
    assert!(base.clone().preserve_atimes(true).requires_apply());
    assert!(base.clone().preserve_crtimes(true).requires_apply());
    assert!(base.clone().fake_super(true).requires_apply());
    assert!(base.clone().with_owner_override(Some(0)).requires_apply());
    assert!(base.clone().with_group_override(Some(0)).requires_apply());
    assert!(
        base.clone()
            .with_chmod(Some(ChmodModifiers::parse("u+x").unwrap()))
            .requires_apply()
    );
}
