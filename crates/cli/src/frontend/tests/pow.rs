use super::common::*;
use super::*;

#[test]
fn pow_u128_for_size_accepts_zero_exponent() {
    let result = pow_u128_for_size(1024, 0).expect("pow for zero exponent");
    assert_eq!(result, 1);
}

#[test]
fn pow_u128_for_size_reports_overflow() {
    let result = pow_u128_for_size(u32::MAX, 5);
    assert!(matches!(result, Err(SizeParseError::TooLarge)));
}
