use super::LineMode;

#[test]
fn line_mode_conversions_round_trip() {
    assert_eq!(LineMode::from(true), LineMode::WithNewline);
    assert_eq!(LineMode::from(false), LineMode::WithoutNewline);

    let append: bool = LineMode::WithNewline.into();
    assert!(append);

    let append: bool = LineMode::WithoutNewline.into();
    assert!(!append);
}
