use super::*;
use std::collections::HashSet;

#[test]
fn log_codes_are_hashable() {
    let mut set = HashSet::new();
    assert!(set.insert(LogCode::Info));
    assert!(set.contains(&LogCode::Info));
    assert!(!set.insert(LogCode::Info));
}

#[test]
fn message_codes_are_hashable() {
    let mut set = HashSet::new();
    assert!(set.insert(MessageCode::Data));
    assert!(set.contains(&MessageCode::Data));
    assert!(!set.insert(MessageCode::Data));
}

#[test]
fn message_code_variants_round_trip_through_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        let decoded = MessageCode::try_from(raw).expect("known code");
        assert_eq!(decoded, code);
    }
}

#[test]
fn message_code_into_u8_matches_as_u8() {
    for &code in MessageCode::all() {
        let converted: u8 = code.into();
        assert_eq!(converted, code.as_u8());
    }
}

#[test]
fn message_code_from_u8_matches_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        assert_eq!(MessageCode::from_u8(raw), Some(code));
        assert_eq!(MessageCode::try_from(raw).ok(), MessageCode::from_u8(raw));
    }
}

#[test]
fn message_code_from_u8_rejects_unknown_values() {
    assert_eq!(MessageCode::from_u8(11), None);
    assert_eq!(MessageCode::from_u8(0xFF), None);
}

#[test]
fn message_code_from_str_parses_known_names() {
    for &code in MessageCode::all() {
        let parsed: MessageCode = code.name().parse().expect("known name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn message_code_from_str_rejects_unknown_names() {
    let err = "MSG_SOMETHING_ELSE".parse::<MessageCode>().unwrap_err();
    assert_eq!(err.invalid_name(), "MSG_SOMETHING_ELSE");
    assert_eq!(
        err.to_string(),
        "unknown multiplexed message code name: \"MSG_SOMETHING_ELSE\""
    );
}

#[test]
fn message_code_all_is_sorted_by_numeric_value() {
    let all = MessageCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "MessageCode::all() is not sorted: {:?}",
            all
        );
    }
}

#[test]
fn logging_classification_matches_upstream_set() {
    const LOGGING_CODES: &[MessageCode] = &[
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::ErrorSocket,
        MessageCode::ErrorUtf8,
        MessageCode::Log,
        MessageCode::Client,
    ];

    for &code in MessageCode::all() {
        let expected = LOGGING_CODES.contains(&code);
        assert_eq!(code.is_logging(), expected, "mismatch for code {code:?}");
    }
}

#[test]
fn message_code_name_matches_upstream_identifiers() {
    use super::MessageCode::*;

    let expected = [
        (Data, "MSG_DATA"),
        (ErrorXfer, "MSG_ERROR_XFER"),
        (Info, "MSG_INFO"),
        (Error, "MSG_ERROR"),
        (Warning, "MSG_WARNING"),
        (ErrorSocket, "MSG_ERROR_SOCKET"),
        (Log, "MSG_LOG"),
        (Client, "MSG_CLIENT"),
        (ErrorUtf8, "MSG_ERROR_UTF8"),
        (Redo, "MSG_REDO"),
        (Stats, "MSG_STATS"),
        (IoError, "MSG_IO_ERROR"),
        (IoTimeout, "MSG_IO_TIMEOUT"),
        (NoOp, "MSG_NOOP"),
        (ErrorExit, "MSG_ERROR_EXIT"),
        (Success, "MSG_SUCCESS"),
        (Deleted, "MSG_DELETED"),
        (NoSend, "MSG_NO_SEND"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}

#[test]
fn message_code_flush_alias_matches_info() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());

    let parsed: MessageCode = "MSG_FLUSH".parse().expect("known alias");
    assert_eq!(parsed, MessageCode::Info);
}

#[test]
fn log_code_all_is_sorted_by_numeric_value() {
    let all = LogCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "LogCode::all() unsorted: {all:?}"
        );
    }
}

#[test]
fn log_code_from_u8_matches_try_from() {
    for &code in LogCode::all() {
        let raw = code.as_u8();
        assert_eq!(LogCode::from_u8(raw), Some(code));
        assert_eq!(LogCode::try_from(raw).ok(), LogCode::from_u8(raw));
    }
}

#[test]
fn log_code_from_u8_rejects_unknown_values() {
    assert_eq!(LogCode::from_u8(9), None);
    let err = LogCode::try_from(9).unwrap_err();
    assert_eq!(err.invalid_value(), Some(9));
    assert_eq!(err.to_string(), "unknown log code value: 9");
}

#[test]
fn log_code_from_str_parses_known_names() {
    for &code in LogCode::all() {
        let parsed: LogCode = code.name().parse().expect("known log code name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn log_code_from_str_rejects_unknown_names() {
    let err = "FUNKNOWN".parse::<LogCode>().unwrap_err();
    assert_eq!(err.invalid_name(), Some("FUNKNOWN"));
    assert_eq!(err.to_string(), "unknown log code name: \"FUNKNOWN\"");
    assert_eq!(err.invalid_value(), None);
}

#[test]
fn log_code_name_matches_upstream_identifiers() {
    use super::LogCode::*;

    let expected = [
        (None, "FNONE"),
        (ErrorXfer, "FERROR_XFER"),
        (Info, "FINFO"),
        (Error, "FERROR"),
        (Warning, "FWARNING"),
        (ErrorSocket, "FERROR_SOCKET"),
        (Log, "FLOG"),
        (Client, "FCLIENT"),
        (ErrorUtf8, "FERROR_UTF8"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}
