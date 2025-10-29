use super::*;

#[test]
fn try_from_log_code_converts_logging_variants() {
    for &log in LogCode::all() {
        match log {
            LogCode::None => {
                let err = MessageCode::try_from(log).expect_err("FNONE has no multiplexed tag");
                assert_eq!(
                    err,
                    LogCodeConversionError::NoMessageEquivalent(LogCode::None)
                );
            }
            _ => {
                let message = MessageCode::try_from(log).expect("log code has multiplexed tag");
                assert_eq!(message.log_code(), Some(log));
            }
        }
    }
}

#[test]
fn try_from_message_code_rejects_non_logging_variants() {
    for &code in MessageCode::all() {
        match code.log_code() {
            Some(log) => {
                let parsed = LogCode::try_from(code).expect("logging code maps to log code");
                assert_eq!(parsed, log);
            }
            None => {
                let err = LogCode::try_from(code).expect_err("non-logging message lacks log code");
                assert_eq!(err, LogCodeConversionError::NoLogEquivalent(code));
            }
        }
    }
}

#[test]
fn message_code_log_code_matches_logging_subset() {
    for &code in MessageCode::all() {
        let log_code = code.log_code();
        assert_eq!(
            log_code.is_some(),
            code.is_logging(),
            "mismatch for {code:?}"
        );

        if let Some(mapped) = log_code {
            assert!(matches!(
                mapped,
                LogCode::ErrorXfer
                    | LogCode::Info
                    | LogCode::Error
                    | LogCode::Warning
                    | LogCode::ErrorSocket
                    | LogCode::Log
                    | LogCode::Client
                    | LogCode::ErrorUtf8,
            ));
        }
    }
}

#[test]
fn message_code_from_log_code_round_trips_logging_variants() {
    for &log in LogCode::all() {
        match MessageCode::from_log_code(log) {
            Some(code) => {
                assert_eq!(code.log_code(), Some(log), "round-trip failed for {log:?}");
            }
            None => assert_eq!(log, LogCode::None, "only FNONE lacks a multiplexed tag"),
        }
    }
}

#[test]
fn message_code_from_log_code_rejects_none_variant() {
    assert_eq!(MessageCode::from_log_code(LogCode::None), None);
}

#[test]
fn try_from_log_code_maps_logging_variants() {
    for &log in LogCode::all() {
        match MessageCode::try_from(log) {
            Ok(code) => assert_eq!(code.log_code(), Some(log)),
            Err(err) => {
                assert_eq!(log, LogCode::None);
                assert_eq!(err.log_code(), Some(log));
                assert!(err.message_code().is_none());
                assert_eq!(
                    err.to_string(),
                    "log code FNONE has no multiplexed message equivalent"
                );
            }
        }
    }
}

#[test]
fn try_from_message_code_requires_logging_equivalent() {
    for &code in MessageCode::all() {
        match LogCode::try_from(code) {
            Ok(log) => assert_eq!(MessageCode::from_log_code(log), Some(code)),
            Err(err) => {
                assert!(code.log_code().is_none());
                assert_eq!(err.message_code(), Some(code));
                assert!(err.log_code().is_none());
                assert_eq!(
                    err.to_string(),
                    format!("message code {code} has no log code equivalent")
                );
            }
        }
    }
}
