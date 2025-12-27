use std::str;

pub(super) fn encode_unsigned_decimal(value: u64, buf: &mut [u8]) -> &str {
    let start = encode_unsigned_decimal_into(value, buf);
    str::from_utf8(&buf[start..]).expect("decimal digits are valid ASCII")
}

pub(super) fn encode_signed_decimal(value: i64, buf: &mut [u8]) -> &str {
    if value < 0 {
        assert!(
            buf.len() >= 2,
            "buffer must include capacity for a sign and at least one digit",
        );

        let start = encode_unsigned_decimal_into(value.unsigned_abs(), buf);
        assert!(
            start > 0,
            "buffer must retain one byte to prefix the minus sign",
        );

        let sign_index = start - 1;
        buf[sign_index] = b'-';
        str::from_utf8(&buf[sign_index..]).expect("decimal digits are valid ASCII")
    } else {
        encode_unsigned_decimal(value as u64, buf)
    }
}

pub(super) fn encode_unsigned_decimal_into(mut value: u64, buf: &mut [u8]) -> usize {
    assert!(
        !buf.is_empty(),
        "buffer must have capacity for at least one digit",
    );

    let mut index = buf.len();
    loop {
        assert!(
            index > 0,
            "decimal representation does not fit in the provided buffer",
        );

        index -= 1;
        buf[index] = b'0' + (value % 10) as u8;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    index
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for encode_unsigned_decimal
    #[test]
    fn encode_unsigned_decimal_zero() {
        let mut buf = [0u8; 20];
        let result = encode_unsigned_decimal(0, &mut buf);
        assert_eq!(result, "0");
    }

    #[test]
    fn encode_unsigned_decimal_single_digit() {
        let mut buf = [0u8; 20];
        let result = encode_unsigned_decimal(5, &mut buf);
        assert_eq!(result, "5");
    }

    #[test]
    fn encode_unsigned_decimal_two_digits() {
        let mut buf = [0u8; 20];
        let result = encode_unsigned_decimal(42, &mut buf);
        assert_eq!(result, "42");
    }

    #[test]
    fn encode_unsigned_decimal_large_number() {
        let mut buf = [0u8; 20];
        let result = encode_unsigned_decimal(123456789, &mut buf);
        assert_eq!(result, "123456789");
    }

    #[test]
    fn encode_unsigned_decimal_max_u64() {
        let mut buf = [0u8; 20];
        let result = encode_unsigned_decimal(u64::MAX, &mut buf);
        assert_eq!(result, "18446744073709551615");
    }

    #[test]
    fn encode_unsigned_decimal_powers_of_ten() {
        let mut buf = [0u8; 20];
        assert_eq!(encode_unsigned_decimal(10, &mut buf), "10");
        assert_eq!(encode_unsigned_decimal(100, &mut buf), "100");
        assert_eq!(encode_unsigned_decimal(1000, &mut buf), "1000");
    }

    // Tests for encode_signed_decimal
    #[test]
    fn encode_signed_decimal_zero() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(0, &mut buf);
        assert_eq!(result, "0");
    }

    #[test]
    fn encode_signed_decimal_positive() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(42, &mut buf);
        assert_eq!(result, "42");
    }

    #[test]
    fn encode_signed_decimal_negative_single_digit() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(-5, &mut buf);
        assert_eq!(result, "-5");
    }

    #[test]
    fn encode_signed_decimal_negative_multi_digit() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(-123, &mut buf);
        assert_eq!(result, "-123");
    }

    #[test]
    fn encode_signed_decimal_negative_large() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(-987654321, &mut buf);
        assert_eq!(result, "-987654321");
    }

    #[test]
    fn encode_signed_decimal_max_i64() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(i64::MAX, &mut buf);
        assert_eq!(result, "9223372036854775807");
    }

    #[test]
    fn encode_signed_decimal_min_i64() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(i64::MIN, &mut buf);
        assert_eq!(result, "-9223372036854775808");
    }

    #[test]
    fn encode_signed_decimal_minus_one() {
        let mut buf = [0u8; 21];
        let result = encode_signed_decimal(-1, &mut buf);
        assert_eq!(result, "-1");
    }

    // Tests for encode_unsigned_decimal_into
    #[test]
    fn encode_unsigned_decimal_into_returns_correct_start_index() {
        let mut buf = [0u8; 10];
        let start = encode_unsigned_decimal_into(42, &mut buf);
        assert_eq!(start, 8); // "42" is 2 digits, so starts at index 8 of 10
    }

    #[test]
    fn encode_unsigned_decimal_into_single_digit_at_end() {
        let mut buf = [0u8; 5];
        let start = encode_unsigned_decimal_into(7, &mut buf);
        assert_eq!(start, 4);
        assert_eq!(buf[4], b'7');
    }

    #[test]
    fn encode_unsigned_decimal_into_fills_buffer() {
        let mut buf = [0u8; 3];
        let start = encode_unsigned_decimal_into(123, &mut buf);
        assert_eq!(start, 0);
        assert_eq!(&buf, b"123");
    }

    // Edge case tests
    #[test]
    fn encode_unsigned_decimal_exact_buffer_size() {
        let mut buf = [0u8; 1];
        let result = encode_unsigned_decimal(9, &mut buf);
        assert_eq!(result, "9");
    }

    #[test]
    fn encode_signed_decimal_exact_buffer_for_negative() {
        let mut buf = [0u8; 2];
        let result = encode_signed_decimal(-1, &mut buf);
        assert_eq!(result, "-1");
    }

    #[test]
    #[should_panic(expected = "buffer must have capacity for at least one digit")]
    fn encode_unsigned_decimal_into_empty_buffer_panics() {
        let mut buf: [u8; 0] = [];
        encode_unsigned_decimal_into(0, &mut buf);
    }

    #[test]
    #[should_panic(expected = "decimal representation does not fit")]
    fn encode_unsigned_decimal_into_buffer_too_small_panics() {
        let mut buf = [0u8; 2];
        encode_unsigned_decimal_into(999, &mut buf); // 3 digits don't fit in 2 bytes
    }
}
