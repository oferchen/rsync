use super::*;

#[test]
fn exit_codes_match_upstream() {
    // upstream: errcode.h
    assert_eq!(ExitCode::Ok.as_i32(), 0);
    assert_eq!(ExitCode::Syntax.as_i32(), 1);
    assert_eq!(ExitCode::Protocol.as_i32(), 2);
    assert_eq!(ExitCode::FileSelect.as_i32(), 3);
    assert_eq!(ExitCode::Unsupported.as_i32(), 4);
    assert_eq!(ExitCode::StartClient.as_i32(), 5);
    assert_eq!(ExitCode::LogFileAppend.as_i32(), 6);
    assert_eq!(ExitCode::SocketIo.as_i32(), 10);
    assert_eq!(ExitCode::FileIo.as_i32(), 11);
    assert_eq!(ExitCode::StreamIo.as_i32(), 12);
    assert_eq!(ExitCode::MessageIo.as_i32(), 13);
    assert_eq!(ExitCode::Ipc.as_i32(), 14);
    assert_eq!(ExitCode::Crashed.as_i32(), 15);
    assert_eq!(ExitCode::Terminated.as_i32(), 16);
    assert_eq!(ExitCode::Signal1.as_i32(), 19);
    assert_eq!(ExitCode::Signal.as_i32(), 20);
    assert_eq!(ExitCode::WaitChild.as_i32(), 21);
    assert_eq!(ExitCode::Malloc.as_i32(), 22);
    assert_eq!(ExitCode::PartialTransfer.as_i32(), 23);
    assert_eq!(ExitCode::Vanished.as_i32(), 24);
    assert_eq!(ExitCode::DeleteLimit.as_i32(), 25);
    assert_eq!(ExitCode::Timeout.as_i32(), 30);
    assert_eq!(ExitCode::ConnectionTimeout.as_i32(), 35);
    assert_eq!(ExitCode::CommandFailed.as_i32(), 124);
    assert_eq!(ExitCode::CommandKilled.as_i32(), 125);
    assert_eq!(ExitCode::CommandRun.as_i32(), 126);
    assert_eq!(ExitCode::CommandNotFound.as_i32(), 127);
}

#[test]
fn from_i32_roundtrips() {
    for code in [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::PartialTransfer,
        ExitCode::Timeout,
        ExitCode::CommandNotFound,
    ] {
        let value = code.as_i32();
        assert_eq!(ExitCode::from_i32(value), Some(code));
    }
}

#[test]
fn from_i32_returns_none_for_unknown() {
    assert_eq!(ExitCode::from_i32(-1), None);
    assert_eq!(ExitCode::from_i32(7), None);
    assert_eq!(ExitCode::from_i32(100), None);
    assert_eq!(ExitCode::from_i32(999), None);
}

#[test]
fn is_success_only_for_ok() {
    assert!(ExitCode::Ok.is_success());
    assert!(!ExitCode::Syntax.is_success());
    assert!(!ExitCode::PartialTransfer.is_success());
}

#[test]
fn is_fatal_for_critical_errors() {
    assert!(ExitCode::Protocol.is_fatal());
    assert!(ExitCode::SocketIo.is_fatal());
    assert!(ExitCode::Timeout.is_fatal());
    assert!(ExitCode::Crashed.is_fatal());

    assert!(!ExitCode::Ok.is_fatal());
    assert!(!ExitCode::PartialTransfer.is_fatal());
    assert!(!ExitCode::Syntax.is_fatal());
}

#[test]
fn is_partial_for_partial_errors() {
    assert!(ExitCode::PartialTransfer.is_partial());
    assert!(ExitCode::Vanished.is_partial());
    assert!(ExitCode::DeleteLimit.is_partial());

    assert!(!ExitCode::Ok.is_partial());
    assert!(!ExitCode::Protocol.is_partial());
}

#[test]
fn display_shows_description() {
    assert_eq!(format!("{}", ExitCode::Ok), "success");
    assert_eq!(format!("{}", ExitCode::PartialTransfer), "partial transfer");
}

#[test]
fn into_i32_conversion() {
    let code: i32 = ExitCode::PartialTransfer.into();
    assert_eq!(code, 23);
}

#[test]
fn into_process_exit_code() {
    let code: std::process::ExitCode = ExitCode::PartialTransfer.into();
    let _ = code;
}

#[test]
fn exit_code_is_copy() {
    let code = ExitCode::Ok;
    let copy = code;
    assert_eq!(code, copy);
}

#[test]
fn exit_code_is_debug() {
    let debug = format!("{:?}", ExitCode::PartialTransfer);
    assert!(debug.contains("PartialTransfer"));
}

#[test]
fn exit_code_is_hashable() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(ExitCode::Ok);
    set.insert(ExitCode::PartialTransfer);
    assert_eq!(set.len(), 2);
}

#[test]
fn descriptions_are_not_empty() {
    for code in [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
        ExitCode::LogFileAppend,
        ExitCode::SocketIo,
        ExitCode::FileIo,
        ExitCode::StreamIo,
        ExitCode::MessageIo,
        ExitCode::Ipc,
        ExitCode::Crashed,
        ExitCode::Terminated,
        ExitCode::Signal1,
        ExitCode::Signal,
        ExitCode::WaitChild,
        ExitCode::Malloc,
        ExitCode::PartialTransfer,
        ExitCode::Vanished,
        ExitCode::DeleteLimit,
        ExitCode::Timeout,
        ExitCode::ConnectionTimeout,
        ExitCode::CommandFailed,
        ExitCode::CommandKilled,
        ExitCode::CommandRun,
        ExitCode::CommandNotFound,
    ] {
        assert!(
            !code.description().is_empty(),
            "Empty description for {code:?}"
        );
    }
}

#[test]
fn from_io_error_maps_file_errors() {
    use std::io::{Error, ErrorKind};

    let err = Error::from(ErrorKind::NotFound);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);

    let err = Error::from(ErrorKind::PermissionDenied);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);

    let err = Error::from(ErrorKind::AlreadyExists);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);
}

#[test]
fn from_io_error_maps_network_errors() {
    use std::io::{Error, ErrorKind};

    let err = Error::from(ErrorKind::ConnectionRefused);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::ConnectionReset);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::ConnectionAborted);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::BrokenPipe);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::AddrInUse);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::AddrNotAvailable);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

    let err = Error::from(ErrorKind::NotConnected);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);
}

#[test]
fn from_io_error_maps_timeout_errors() {
    use std::io::{Error, ErrorKind};

    let err = Error::from(ErrorKind::TimedOut);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::Timeout);
}

#[test]
fn from_io_error_maps_stream_errors() {
    use std::io::{Error, ErrorKind};

    let err = Error::from(ErrorKind::UnexpectedEof);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::StreamIo);

    let err = Error::from(ErrorKind::InvalidData);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::StreamIo);
}

#[test]
fn from_io_error_maps_signal_interruption() {
    use std::io::{Error, ErrorKind};

    let err = Error::from(ErrorKind::Interrupted);
    assert_eq!(ExitCode::from_io_error(&err), ExitCode::Signal);
}

#[test]
fn from_io_error_defaults_to_file_io() {
    use std::io::{Error, ErrorKind};

    for kind in [
        ErrorKind::WriteZero,
        ErrorKind::InvalidInput,
        ErrorKind::Other,
        ErrorKind::OutOfMemory,
    ] {
        let err = Error::from(kind);
        assert_eq!(
            ExitCode::from_io_error(&err),
            ExitCode::FileIo,
            "ErrorKind::{kind:?} should map to FileIo"
        );
    }
}

#[test]
fn exit_code_description_function_works() {
    assert_eq!(exit_code_description(0), "success");
    assert_eq!(exit_code_description(1), "syntax or usage error");
    assert_eq!(exit_code_description(23), "partial transfer");
    assert_eq!(
        exit_code_description(6),
        "daemon unable to append to log-file"
    );
}

#[test]
fn exit_code_description_handles_unknown() {
    assert_eq!(exit_code_description(999), "unknown error code: 999");
    assert_eq!(exit_code_description(-1), "unknown error code: -1");
    assert_eq!(exit_code_description(7), "unknown error code: 7");
}

#[test]
fn all_upstream_exit_codes_present() {
    // upstream: errcode.h - verify all codes from rsync 3.4.1
    assert_eq!(ExitCode::Ok.as_i32(), 0, "Success");
    assert_eq!(ExitCode::Syntax.as_i32(), 1, "Syntax or usage error");
    assert_eq!(ExitCode::Protocol.as_i32(), 2, "Protocol incompatibility");
    assert_eq!(
        ExitCode::FileSelect.as_i32(),
        3,
        "Errors selecting input/output files, dirs"
    );
    assert_eq!(
        ExitCode::Unsupported.as_i32(),
        4,
        "Requested action not supported"
    );
    assert_eq!(
        ExitCode::StartClient.as_i32(),
        5,
        "Error starting client-server protocol"
    );
    assert_eq!(
        ExitCode::LogFileAppend.as_i32(),
        6,
        "Daemon unable to append to log-file"
    );
    assert_eq!(ExitCode::SocketIo.as_i32(), 10, "Error in socket I/O");
    assert_eq!(ExitCode::FileIo.as_i32(), 11, "Error in file I/O");
    assert_eq!(
        ExitCode::StreamIo.as_i32(),
        12,
        "Error in rsync protocol data stream"
    );
    assert_eq!(
        ExitCode::MessageIo.as_i32(),
        13,
        "Errors with program diagnostics"
    );
    assert_eq!(ExitCode::Ipc.as_i32(), 14, "Error in IPC code");
    assert_eq!(ExitCode::Signal.as_i32(), 20, "Received SIGUSR1 or SIGINT");
    assert_eq!(
        ExitCode::WaitChild.as_i32(),
        21,
        "Some error returned by waitpid()"
    );
    assert_eq!(
        ExitCode::Malloc.as_i32(),
        22,
        "Error allocating core memory buffers"
    );
    assert_eq!(
        ExitCode::PartialTransfer.as_i32(),
        23,
        "Partial transfer due to error"
    );
    assert_eq!(
        ExitCode::Vanished.as_i32(),
        24,
        "Partial transfer due to vanished source files"
    );
    assert_eq!(
        ExitCode::DeleteLimit.as_i32(),
        25,
        "The --max-delete limit stopped deletions"
    );
    assert_eq!(
        ExitCode::Timeout.as_i32(),
        30,
        "Timeout in data send/receive"
    );
    assert_eq!(
        ExitCode::ConnectionTimeout.as_i32(),
        35,
        "Timeout waiting for daemon connection"
    );
}

#[test]
fn from_i32_covers_all_exit_codes() {
    let all_codes = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
        ExitCode::LogFileAppend,
        ExitCode::SocketIo,
        ExitCode::FileIo,
        ExitCode::StreamIo,
        ExitCode::MessageIo,
        ExitCode::Ipc,
        ExitCode::Crashed,
        ExitCode::Terminated,
        ExitCode::Signal1,
        ExitCode::Signal,
        ExitCode::WaitChild,
        ExitCode::Malloc,
        ExitCode::PartialTransfer,
        ExitCode::Vanished,
        ExitCode::DeleteLimit,
        ExitCode::Timeout,
        ExitCode::ConnectionTimeout,
        ExitCode::CommandFailed,
        ExitCode::CommandKilled,
        ExitCode::CommandRun,
        ExitCode::CommandNotFound,
    ];

    for code in all_codes {
        let value = code.as_i32();
        assert_eq!(
            ExitCode::from_i32(value),
            Some(code),
            "Failed to round-trip {code:?} (value: {value})"
        );
    }
}
