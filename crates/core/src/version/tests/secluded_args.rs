use super::*;
use ::core::str::FromStr;

#[test]
fn secluded_args_mode_labels_round_trip() {
    assert_eq!(
        SecludedArgsMode::from_label(SecludedArgsMode::Optional.label()),
        Some(SecludedArgsMode::Optional)
    );
    assert_eq!(
        SecludedArgsMode::from_label(SecludedArgsMode::Default.label()),
        Some(SecludedArgsMode::Default)
    );
    assert!(SecludedArgsMode::from_label("custom secluded-args").is_none());
}

#[test]
fn secluded_args_mode_display_matches_label() {
    assert_eq!(
        SecludedArgsMode::Optional.to_string(),
        SecludedArgsMode::Optional.label()
    );
    assert_eq!(
        SecludedArgsMode::Default.to_string(),
        SecludedArgsMode::Default.label()
    );
}

#[test]
fn secluded_args_mode_from_str_rejects_unknown_values() {
    assert_eq!(
        SecludedArgsMode::from_str("default secluded-args"),
        Ok(SecludedArgsMode::Default)
    );
    assert_eq!(
        SecludedArgsMode::from_str("optional secluded-args"),
        Ok(SecludedArgsMode::Optional)
    );
    assert!(SecludedArgsMode::from_str("disabled secluded-args").is_err());
}
