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
