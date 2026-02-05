//! Comprehensive exit code tests matching upstream rsync behavior.
//!
//! This test suite verifies that the ExitCode enum and related traits correctly
//! implement upstream rsync's exit code definitions from errcode.h.
//!
//! Tests are organized by exit code value and cover:
//! - Correct numeric values matching upstream
//! - Description strings matching upstream log.c
//! - Roundtrip conversion (as_i32 <-> from_i32)
//! - Exit code classification (is_success, is_fatal, is_partial)
//! - Trait implementations (Display, From, HasExitCode)
//!
//! Reference: rsync 3.4.1 errcode.h and log.c

use core::exit_code::{ErrorCodification, ExitCode, HasExitCode};
use std::collections::HashSet;

// ============================================================================
// Exit Code 0: Success (RERR_OK)
// ============================================================================

#[test]
fn exit_code_0_is_ok() {
    assert_eq!(ExitCode::Ok.as_i32(), 0);
    assert_eq!(ExitCode::from_i32(0), Some(ExitCode::Ok));
}

#[test]
fn exit_code_0_description() {
    assert_eq!(ExitCode::Ok.description(), "success");
    assert_eq!(format!("{}", ExitCode::Ok), "success");
}

#[test]
fn exit_code_0_is_success() {
    assert!(ExitCode::Ok.is_success());
    assert!(!ExitCode::Ok.is_fatal());
    assert!(!ExitCode::Ok.is_partial());
}

// ============================================================================
// Exit Code 1: Syntax Error (RERR_SYNTAX)
// ============================================================================

#[test]
fn exit_code_1_is_syntax() {
    assert_eq!(ExitCode::Syntax.as_i32(), 1);
    assert_eq!(ExitCode::from_i32(1), Some(ExitCode::Syntax));
}

#[test]
fn exit_code_1_description() {
    assert_eq!(ExitCode::Syntax.description(), "syntax or usage error");
    assert_eq!(format!("{}", ExitCode::Syntax), "syntax or usage error");
}

#[test]
fn exit_code_1_classification() {
    assert!(!ExitCode::Syntax.is_success());
    assert!(!ExitCode::Syntax.is_fatal());
    assert!(!ExitCode::Syntax.is_partial());
}

// ============================================================================
// Exit Code 2: Protocol Incompatibility (RERR_PROTOCOL)
// ============================================================================

#[test]
fn exit_code_2_is_protocol() {
    assert_eq!(ExitCode::Protocol.as_i32(), 2);
    assert_eq!(ExitCode::from_i32(2), Some(ExitCode::Protocol));
}

#[test]
fn exit_code_2_description() {
    assert_eq!(ExitCode::Protocol.description(), "protocol incompatibility");
    assert_eq!(format!("{}", ExitCode::Protocol), "protocol incompatibility");
}

#[test]
fn exit_code_2_is_fatal() {
    assert!(!ExitCode::Protocol.is_success());
    assert!(ExitCode::Protocol.is_fatal());
    assert!(!ExitCode::Protocol.is_partial());
}

// ============================================================================
// Exit Code 3: File Selection Error (RERR_FILESELECT)
// ============================================================================

#[test]
fn exit_code_3_is_file_select() {
    assert_eq!(ExitCode::FileSelect.as_i32(), 3);
    assert_eq!(ExitCode::from_i32(3), Some(ExitCode::FileSelect));
}

#[test]
fn exit_code_3_description() {
    assert_eq!(
        ExitCode::FileSelect.description(),
        "errors selecting input/output files, dirs"
    );
}

#[test]
fn exit_code_3_classification() {
    assert!(!ExitCode::FileSelect.is_success());
    assert!(!ExitCode::FileSelect.is_fatal());
    assert!(!ExitCode::FileSelect.is_partial());
}

// ============================================================================
// Exit Code 4: Unsupported Action (RERR_UNSUPPORTED)
// ============================================================================

#[test]
fn exit_code_4_is_unsupported() {
    assert_eq!(ExitCode::Unsupported.as_i32(), 4);
    assert_eq!(ExitCode::from_i32(4), Some(ExitCode::Unsupported));
}

#[test]
fn exit_code_4_description() {
    assert_eq!(
        ExitCode::Unsupported.description(),
        "requested action not supported"
    );
}

#[test]
fn exit_code_4_classification() {
    assert!(!ExitCode::Unsupported.is_success());
    assert!(!ExitCode::Unsupported.is_fatal());
    assert!(!ExitCode::Unsupported.is_partial());
}

// ============================================================================
// Exit Code 5: Start Client Error (RERR_STARTCLIENT)
// ============================================================================

#[test]
fn exit_code_5_is_start_client() {
    assert_eq!(ExitCode::StartClient.as_i32(), 5);
    assert_eq!(ExitCode::from_i32(5), Some(ExitCode::StartClient));
}

#[test]
fn exit_code_5_description() {
    assert_eq!(
        ExitCode::StartClient.description(),
        "error starting client-server protocol"
    );
}

#[test]
fn exit_code_5_is_fatal() {
    assert!(!ExitCode::StartClient.is_success());
    assert!(ExitCode::StartClient.is_fatal());
    assert!(!ExitCode::StartClient.is_partial());
}

// ============================================================================
// Exit Code 10: Socket I/O Error (RERR_SOCKETIO)
// ============================================================================

#[test]
fn exit_code_10_is_socket_io() {
    assert_eq!(ExitCode::SocketIo.as_i32(), 10);
    assert_eq!(ExitCode::from_i32(10), Some(ExitCode::SocketIo));
}

#[test]
fn exit_code_10_description() {
    assert_eq!(ExitCode::SocketIo.description(), "error in socket IO");
}

#[test]
fn exit_code_10_is_fatal() {
    assert!(!ExitCode::SocketIo.is_success());
    assert!(ExitCode::SocketIo.is_fatal());
    assert!(!ExitCode::SocketIo.is_partial());
}

// ============================================================================
// Exit Code 11: File I/O Error (RERR_FILEIO)
// ============================================================================

#[test]
fn exit_code_11_is_file_io() {
    assert_eq!(ExitCode::FileIo.as_i32(), 11);
    assert_eq!(ExitCode::from_i32(11), Some(ExitCode::FileIo));
}

#[test]
fn exit_code_11_description() {
    assert_eq!(ExitCode::FileIo.description(), "error in file IO");
}

#[test]
fn exit_code_11_classification() {
    assert!(!ExitCode::FileIo.is_success());
    assert!(!ExitCode::FileIo.is_fatal());
    assert!(!ExitCode::FileIo.is_partial());
}

// ============================================================================
// Exit Code 12: Stream I/O Error (RERR_STREAMIO)
// ============================================================================

#[test]
fn exit_code_12_is_stream_io() {
    assert_eq!(ExitCode::StreamIo.as_i32(), 12);
    assert_eq!(ExitCode::from_i32(12), Some(ExitCode::StreamIo));
}

#[test]
fn exit_code_12_description() {
    assert_eq!(
        ExitCode::StreamIo.description(),
        "error in rsync protocol data stream"
    );
}

#[test]
fn exit_code_12_is_fatal() {
    assert!(!ExitCode::StreamIo.is_success());
    assert!(ExitCode::StreamIo.is_fatal());
    assert!(!ExitCode::StreamIo.is_partial());
}

// ============================================================================
// Exit Code 13: Message I/O Error (RERR_MESSAGEIO)
// ============================================================================

#[test]
fn exit_code_13_is_message_io() {
    assert_eq!(ExitCode::MessageIo.as_i32(), 13);
    assert_eq!(ExitCode::from_i32(13), Some(ExitCode::MessageIo));
}

#[test]
fn exit_code_13_description() {
    assert_eq!(
        ExitCode::MessageIo.description(),
        "errors with program diagnostics"
    );
}

#[test]
fn exit_code_13_classification() {
    assert!(!ExitCode::MessageIo.is_success());
    assert!(!ExitCode::MessageIo.is_fatal());
    assert!(!ExitCode::MessageIo.is_partial());
}

// ============================================================================
// Exit Code 14: IPC Error (RERR_IPC)
// ============================================================================

#[test]
fn exit_code_14_is_ipc() {
    assert_eq!(ExitCode::Ipc.as_i32(), 14);
    assert_eq!(ExitCode::from_i32(14), Some(ExitCode::Ipc));
}

#[test]
fn exit_code_14_description() {
    assert_eq!(ExitCode::Ipc.description(), "error in IPC code");
}

#[test]
fn exit_code_14_is_fatal() {
    assert!(!ExitCode::Ipc.is_success());
    assert!(ExitCode::Ipc.is_fatal());
    assert!(!ExitCode::Ipc.is_partial());
}

// ============================================================================
// Exit Code 15: Crashed (RERR_CRASHED)
// ============================================================================

#[test]
fn exit_code_15_is_crashed() {
    assert_eq!(ExitCode::Crashed.as_i32(), 15);
    assert_eq!(ExitCode::from_i32(15), Some(ExitCode::Crashed));
}

#[test]
fn exit_code_15_description() {
    assert_eq!(
        ExitCode::Crashed.description(),
        "received SIGSEGV or SIGBUS or SIGABRT"
    );
}

#[test]
fn exit_code_15_is_fatal() {
    assert!(!ExitCode::Crashed.is_success());
    assert!(ExitCode::Crashed.is_fatal());
    assert!(!ExitCode::Crashed.is_partial());
}

// ============================================================================
// Exit Code 16: Terminated (RERR_TERMINATED)
// ============================================================================

#[test]
fn exit_code_16_is_terminated() {
    assert_eq!(ExitCode::Terminated.as_i32(), 16);
    assert_eq!(ExitCode::from_i32(16), Some(ExitCode::Terminated));
}

#[test]
fn exit_code_16_description() {
    assert_eq!(
        ExitCode::Terminated.description(),
        "received SIGINT, SIGTERM, or SIGHUP"
    );
}

#[test]
fn exit_code_16_is_fatal() {
    assert!(!ExitCode::Terminated.is_success());
    assert!(ExitCode::Terminated.is_fatal());
    assert!(!ExitCode::Terminated.is_partial());
}

// ============================================================================
// Exit Code 19: Signal1 (RERR_SIGNAL1)
// ============================================================================

#[test]
fn exit_code_19_is_signal1() {
    assert_eq!(ExitCode::Signal1.as_i32(), 19);
    assert_eq!(ExitCode::from_i32(19), Some(ExitCode::Signal1));
}

#[test]
fn exit_code_19_description() {
    assert_eq!(ExitCode::Signal1.description(), "received SIGUSR1");
}

#[test]
fn exit_code_19_classification() {
    assert!(!ExitCode::Signal1.is_success());
    assert!(!ExitCode::Signal1.is_fatal());
    assert!(!ExitCode::Signal1.is_partial());
}

// ============================================================================
// Exit Code 20: Signal (RERR_SIGNAL)
// ============================================================================

#[test]
fn exit_code_20_is_signal() {
    assert_eq!(ExitCode::Signal.as_i32(), 20);
    assert_eq!(ExitCode::from_i32(20), Some(ExitCode::Signal));
}

#[test]
fn exit_code_20_description() {
    assert_eq!(
        ExitCode::Signal.description(),
        "received SIGINT, SIGTERM, or SIGHUP"
    );
}

#[test]
fn exit_code_20_classification() {
    assert!(!ExitCode::Signal.is_success());
    assert!(!ExitCode::Signal.is_fatal());
    assert!(!ExitCode::Signal.is_partial());
}

// ============================================================================
// Exit Code 21: WaitChild (RERR_WAITCHILD)
// ============================================================================

#[test]
fn exit_code_21_is_wait_child() {
    assert_eq!(ExitCode::WaitChild.as_i32(), 21);
    assert_eq!(ExitCode::from_i32(21), Some(ExitCode::WaitChild));
}

#[test]
fn exit_code_21_description() {
    assert_eq!(ExitCode::WaitChild.description(), "waitpid() failed");
}

#[test]
fn exit_code_21_classification() {
    assert!(!ExitCode::WaitChild.is_success());
    assert!(!ExitCode::WaitChild.is_fatal());
    assert!(!ExitCode::WaitChild.is_partial());
}

// ============================================================================
// Exit Code 22: Malloc (RERR_MALLOC)
// ============================================================================

#[test]
fn exit_code_22_is_malloc() {
    assert_eq!(ExitCode::Malloc.as_i32(), 22);
    assert_eq!(ExitCode::from_i32(22), Some(ExitCode::Malloc));
}

#[test]
fn exit_code_22_description() {
    assert_eq!(
        ExitCode::Malloc.description(),
        "error allocating core memory buffers"
    );
}

#[test]
fn exit_code_22_is_fatal() {
    assert!(!ExitCode::Malloc.is_success());
    assert!(ExitCode::Malloc.is_fatal());
    assert!(!ExitCode::Malloc.is_partial());
}

// ============================================================================
// Exit Code 23: Partial Transfer (RERR_PARTIAL)
// ============================================================================

#[test]
fn exit_code_23_is_partial_transfer() {
    assert_eq!(ExitCode::PartialTransfer.as_i32(), 23);
    assert_eq!(ExitCode::from_i32(23), Some(ExitCode::PartialTransfer));
}

#[test]
fn exit_code_23_description() {
    assert_eq!(ExitCode::PartialTransfer.description(), "partial transfer");
}

#[test]
fn exit_code_23_is_partial() {
    assert!(!ExitCode::PartialTransfer.is_success());
    assert!(!ExitCode::PartialTransfer.is_fatal());
    assert!(ExitCode::PartialTransfer.is_partial());
}

// ============================================================================
// Exit Code 24: Vanished (RERR_VANISHED)
// ============================================================================

#[test]
fn exit_code_24_is_vanished() {
    assert_eq!(ExitCode::Vanished.as_i32(), 24);
    assert_eq!(ExitCode::from_i32(24), Some(ExitCode::Vanished));
}

#[test]
fn exit_code_24_description() {
    assert_eq!(
        ExitCode::Vanished.description(),
        "some files vanished before they could be transferred"
    );
}

#[test]
fn exit_code_24_is_partial() {
    assert!(!ExitCode::Vanished.is_success());
    assert!(!ExitCode::Vanished.is_fatal());
    assert!(ExitCode::Vanished.is_partial());
}

// ============================================================================
// Exit Code 25: Delete Limit (RERR_DEL_LIMIT)
// ============================================================================

#[test]
fn exit_code_25_is_delete_limit() {
    assert_eq!(ExitCode::DeleteLimit.as_i32(), 25);
    assert_eq!(ExitCode::from_i32(25), Some(ExitCode::DeleteLimit));
}

#[test]
fn exit_code_25_description() {
    assert_eq!(
        ExitCode::DeleteLimit.description(),
        "max delete limit stopped deletions"
    );
}

#[test]
fn exit_code_25_is_partial() {
    assert!(!ExitCode::DeleteLimit.is_success());
    assert!(!ExitCode::DeleteLimit.is_fatal());
    assert!(ExitCode::DeleteLimit.is_partial());
}

// ============================================================================
// Exit Code 30: Timeout (RERR_TIMEOUT)
// ============================================================================

#[test]
fn exit_code_30_is_timeout() {
    assert_eq!(ExitCode::Timeout.as_i32(), 30);
    assert_eq!(ExitCode::from_i32(30), Some(ExitCode::Timeout));
}

#[test]
fn exit_code_30_description() {
    assert_eq!(
        ExitCode::Timeout.description(),
        "timeout in data send/receive"
    );
}

#[test]
fn exit_code_30_is_fatal() {
    assert!(!ExitCode::Timeout.is_success());
    assert!(ExitCode::Timeout.is_fatal());
    assert!(!ExitCode::Timeout.is_partial());
}

// ============================================================================
// Exit Code 35: Connection Timeout (RERR_CONTIMEOUT)
// ============================================================================

#[test]
fn exit_code_35_is_connection_timeout() {
    assert_eq!(ExitCode::ConnectionTimeout.as_i32(), 35);
    assert_eq!(ExitCode::from_i32(35), Some(ExitCode::ConnectionTimeout));
}

#[test]
fn exit_code_35_description() {
    assert_eq!(
        ExitCode::ConnectionTimeout.description(),
        "timeout waiting for daemon connection"
    );
}

#[test]
fn exit_code_35_is_fatal() {
    assert!(!ExitCode::ConnectionTimeout.is_success());
    assert!(ExitCode::ConnectionTimeout.is_fatal());
    assert!(!ExitCode::ConnectionTimeout.is_partial());
}

// ============================================================================
// Exit Code 124: Command Failed (RERR_CMD_FAILED)
// ============================================================================

#[test]
fn exit_code_124_is_command_failed() {
    assert_eq!(ExitCode::CommandFailed.as_i32(), 124);
    assert_eq!(ExitCode::from_i32(124), Some(ExitCode::CommandFailed));
}

#[test]
fn exit_code_124_description() {
    assert_eq!(ExitCode::CommandFailed.description(), "remote command failed");
}

#[test]
fn exit_code_124_classification() {
    assert!(!ExitCode::CommandFailed.is_success());
    assert!(!ExitCode::CommandFailed.is_fatal());
    assert!(!ExitCode::CommandFailed.is_partial());
}

// ============================================================================
// Exit Code 125: Command Killed (RERR_CMD_KILLED)
// ============================================================================

#[test]
fn exit_code_125_is_command_killed() {
    assert_eq!(ExitCode::CommandKilled.as_i32(), 125);
    assert_eq!(ExitCode::from_i32(125), Some(ExitCode::CommandKilled));
}

#[test]
fn exit_code_125_description() {
    assert_eq!(
        ExitCode::CommandKilled.description(),
        "remote command killed"
    );
}

#[test]
fn exit_code_125_classification() {
    assert!(!ExitCode::CommandKilled.is_success());
    assert!(!ExitCode::CommandKilled.is_fatal());
    assert!(!ExitCode::CommandKilled.is_partial());
}

// ============================================================================
// Exit Code 126: Command Run (RERR_CMD_RUN)
// ============================================================================

#[test]
fn exit_code_126_is_command_run() {
    assert_eq!(ExitCode::CommandRun.as_i32(), 126);
    assert_eq!(ExitCode::from_i32(126), Some(ExitCode::CommandRun));
}

#[test]
fn exit_code_126_description() {
    assert_eq!(
        ExitCode::CommandRun.description(),
        "remote command could not be run"
    );
}

#[test]
fn exit_code_126_classification() {
    assert!(!ExitCode::CommandRun.is_success());
    assert!(!ExitCode::CommandRun.is_fatal());
    assert!(!ExitCode::CommandRun.is_partial());
}

// ============================================================================
// Exit Code 127: Command Not Found (RERR_CMD_NOTFOUND)
// ============================================================================

#[test]
fn exit_code_127_is_command_not_found() {
    assert_eq!(ExitCode::CommandNotFound.as_i32(), 127);
    assert_eq!(ExitCode::from_i32(127), Some(ExitCode::CommandNotFound));
}

#[test]
fn exit_code_127_description() {
    assert_eq!(
        ExitCode::CommandNotFound.description(),
        "remote command not found"
    );
}

#[test]
fn exit_code_127_classification() {
    assert!(!ExitCode::CommandNotFound.is_success());
    assert!(!ExitCode::CommandNotFound.is_fatal());
    assert!(!ExitCode::CommandNotFound.is_partial());
}

// ============================================================================
// Comprehensive Enumeration Tests
// ============================================================================

#[test]
fn all_exit_codes_have_unique_values() {
    let all_codes = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
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

    let mut values = HashSet::new();
    for code in &all_codes {
        let value = code.as_i32();
        assert!(
            values.insert(value),
            "Duplicate exit code value: {value} for {code:?}"
        );
    }

    // Verify we have all 26 codes
    assert_eq!(values.len(), 26, "Should have exactly 26 unique exit codes");
}

#[test]
fn all_exit_codes_roundtrip() {
    let all_codes = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
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
        let parsed = ExitCode::from_i32(value);
        assert_eq!(
            parsed,
            Some(code),
            "Roundtrip failed for {code:?}: value={value}"
        );
    }
}

#[test]
fn all_exit_codes_have_descriptions() {
    let all_codes = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
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
        let desc = code.description();
        assert!(
            !desc.is_empty(),
            "Exit code {code:?} has empty description"
        );
        assert!(
            desc.len() > 3,
            "Exit code {code:?} has suspiciously short description: {desc}"
        );
    }
}

#[test]
fn unknown_exit_codes_return_none() {
    // Test some invalid codes
    let invalid_codes = [
        -1, 6, 7, 8, 9, 17, 18, 26, 27, 28, 29, 31, 32, 33, 34, 36, 100, 123, 128, 255, 999,
    ];

    for value in invalid_codes {
        assert!(
            ExitCode::from_i32(value).is_none(),
            "from_i32({value}) should return None"
        );
    }
}

// ============================================================================
// Classification Tests
// ============================================================================

#[test]
fn only_ok_is_success() {
    let all_codes = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
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
        if code == ExitCode::Ok {
            assert!(code.is_success(), "{code:?} should be success");
        } else {
            assert!(!code.is_success(), "{code:?} should not be success");
        }
    }
}

#[test]
fn fatal_errors_are_correct() {
    let fatal = [
        ExitCode::Protocol,
        ExitCode::StartClient,
        ExitCode::SocketIo,
        ExitCode::StreamIo,
        ExitCode::Ipc,
        ExitCode::Crashed,
        ExitCode::Terminated,
        ExitCode::Malloc,
        ExitCode::Timeout,
        ExitCode::ConnectionTimeout,
    ];

    let non_fatal = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::FileIo,
        ExitCode::MessageIo,
        ExitCode::Signal1,
        ExitCode::Signal,
        ExitCode::WaitChild,
        ExitCode::PartialTransfer,
        ExitCode::Vanished,
        ExitCode::DeleteLimit,
        ExitCode::CommandFailed,
        ExitCode::CommandKilled,
        ExitCode::CommandRun,
        ExitCode::CommandNotFound,
    ];

    for code in fatal {
        assert!(code.is_fatal(), "{code:?} should be fatal");
    }

    for code in non_fatal {
        assert!(!code.is_fatal(), "{code:?} should not be fatal");
    }
}

#[test]
fn partial_errors_are_correct() {
    let partial = [
        ExitCode::PartialTransfer,
        ExitCode::Vanished,
        ExitCode::DeleteLimit,
    ];

    let non_partial = [
        ExitCode::Ok,
        ExitCode::Syntax,
        ExitCode::Protocol,
        ExitCode::FileSelect,
        ExitCode::Unsupported,
        ExitCode::StartClient,
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
        ExitCode::Timeout,
        ExitCode::ConnectionTimeout,
        ExitCode::CommandFailed,
        ExitCode::CommandKilled,
        ExitCode::CommandRun,
        ExitCode::CommandNotFound,
    ];

    for code in partial {
        assert!(code.is_partial(), "{code:?} should be partial");
    }

    for code in non_partial {
        assert!(!code.is_partial(), "{code:?} should not be partial");
    }
}

// ============================================================================
// Trait Implementation Tests
// ============================================================================

#[test]
fn exit_code_implements_copy() {
    let code = ExitCode::PartialTransfer;
    let copy = code;
    assert_eq!(code, copy);
    // Both should still be usable
    assert_eq!(code.as_i32(), 23);
    assert_eq!(copy.as_i32(), 23);
}

#[test]
fn exit_code_implements_debug() {
    let code = ExitCode::PartialTransfer;
    let debug = format!("{code:?}");
    assert!(
        debug.contains("PartialTransfer"),
        "Debug output should contain variant name"
    );
}

#[test]
fn exit_code_implements_display() {
    let code = ExitCode::PartialTransfer;
    let display = format!("{code}");
    assert_eq!(display, "partial transfer");
}

#[test]
fn exit_code_converts_to_i32() {
    let code = ExitCode::PartialTransfer;
    let value: i32 = code.into();
    assert_eq!(value, 23);
}

#[test]
fn exit_code_converts_to_process_exit_code() {
    let code = ExitCode::PartialTransfer;
    let _process_code: std::process::ExitCode = code.into();
    // Can't directly test the value, but we verify it compiles and converts
}

#[test]
fn exit_code_is_hashable() {
    use std::collections::HashMap;

    let mut map = HashMap::new();
    map.insert(ExitCode::Ok, "success");
    map.insert(ExitCode::PartialTransfer, "partial");
    map.insert(ExitCode::Protocol, "protocol error");

    assert_eq!(map.get(&ExitCode::Ok), Some(&"success"));
    assert_eq!(map.get(&ExitCode::PartialTransfer), Some(&"partial"));
    assert_eq!(map.get(&ExitCode::Protocol), Some(&"protocol error"));
}

// ============================================================================
// ErrorCodification Trait Tests
// ============================================================================

/// Mock error type for testing ErrorCodification trait
#[derive(Debug)]
enum MockError {
    NotFound { path: String },
    PermissionDenied { path: String },
    NetworkError,
}

impl std::fmt::Display for MockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { path } => write!(f, "file not found: {path}"),
            Self::PermissionDenied { path } => write!(f, "permission denied: {path}"),
            Self::NetworkError => write!(f, "network error"),
        }
    }
}

impl HasExitCode for MockError {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::NotFound { .. } => ExitCode::FileSelect,
            Self::PermissionDenied { .. } => ExitCode::FileIo,
            Self::NetworkError => ExitCode::SocketIo,
        }
    }
}

impl ErrorCodification for MockError {
    fn error_code(&self) -> u32 {
        match self {
            Self::NotFound { .. } => 1001,
            Self::PermissionDenied { .. } => 1002,
            Self::NetworkError => 1003,
        }
    }
}

#[test]
fn error_codification_provides_error_code() {
    let err = MockError::NotFound {
        path: "/tmp/test".to_string(),
    };
    assert_eq!(err.error_code(), 1001);

    let err = MockError::PermissionDenied {
        path: "/etc/passwd".to_string(),
    };
    assert_eq!(err.error_code(), 1002);

    let err = MockError::NetworkError;
    assert_eq!(err.error_code(), 1003);
}

#[test]
fn error_codification_provides_exit_code_i32() {
    let err = MockError::NotFound {
        path: "/tmp/test".to_string(),
    };
    assert_eq!(err.exit_code_i32(), 3); // FileSelect

    let err = MockError::PermissionDenied {
        path: "/etc/passwd".to_string(),
    };
    assert_eq!(err.exit_code_i32(), 11); // FileIo

    let err = MockError::NetworkError;
    assert_eq!(err.exit_code_i32(), 10); // SocketIo
}

#[test]
fn error_codification_provides_user_message() {
    let err = MockError::NotFound {
        path: "/tmp/test".to_string(),
    };
    assert_eq!(err.user_message(), "file not found: /tmp/test");

    let err = MockError::PermissionDenied {
        path: "/etc/passwd".to_string(),
    };
    assert_eq!(err.user_message(), "permission denied: /etc/passwd");

    let err = MockError::NetworkError;
    assert_eq!(err.user_message(), "network error");
}

#[test]
fn error_codification_provides_error_code_name() {
    let err = MockError::NotFound {
        path: "/tmp/test".to_string(),
    };
    assert_eq!(
        err.error_code_name(),
        "errors selecting input/output files, dirs"
    );

    let err = MockError::PermissionDenied {
        path: "/etc/passwd".to_string(),
    };
    assert_eq!(err.error_code_name(), "error in file IO");

    let err = MockError::NetworkError;
    assert_eq!(err.error_code_name(), "error in socket IO");
}

// ============================================================================
// Edge Cases and Boundary Tests
// ============================================================================

#[test]
fn exit_code_values_match_upstream_exactly() {
    // This is a comprehensive test that all values match errcode.h
    assert_eq!(ExitCode::Ok.as_i32(), 0);
    assert_eq!(ExitCode::Syntax.as_i32(), 1);
    assert_eq!(ExitCode::Protocol.as_i32(), 2);
    assert_eq!(ExitCode::FileSelect.as_i32(), 3);
    assert_eq!(ExitCode::Unsupported.as_i32(), 4);
    assert_eq!(ExitCode::StartClient.as_i32(), 5);
    // Note: 6-9 are not defined
    assert_eq!(ExitCode::SocketIo.as_i32(), 10);
    assert_eq!(ExitCode::FileIo.as_i32(), 11);
    assert_eq!(ExitCode::StreamIo.as_i32(), 12);
    assert_eq!(ExitCode::MessageIo.as_i32(), 13);
    assert_eq!(ExitCode::Ipc.as_i32(), 14);
    assert_eq!(ExitCode::Crashed.as_i32(), 15);
    assert_eq!(ExitCode::Terminated.as_i32(), 16);
    // Note: 17-18 are not defined
    assert_eq!(ExitCode::Signal1.as_i32(), 19);
    assert_eq!(ExitCode::Signal.as_i32(), 20);
    assert_eq!(ExitCode::WaitChild.as_i32(), 21);
    assert_eq!(ExitCode::Malloc.as_i32(), 22);
    assert_eq!(ExitCode::PartialTransfer.as_i32(), 23);
    assert_eq!(ExitCode::Vanished.as_i32(), 24);
    assert_eq!(ExitCode::DeleteLimit.as_i32(), 25);
    // Note: 26-29 are not defined
    assert_eq!(ExitCode::Timeout.as_i32(), 30);
    // Note: 31-34 are not defined
    assert_eq!(ExitCode::ConnectionTimeout.as_i32(), 35);
    // Note: 36-123 are not defined
    assert_eq!(ExitCode::CommandFailed.as_i32(), 124);
    assert_eq!(ExitCode::CommandKilled.as_i32(), 125);
    assert_eq!(ExitCode::CommandRun.as_i32(), 126);
    assert_eq!(ExitCode::CommandNotFound.as_i32(), 127);
}

#[test]
fn exit_code_descriptions_match_upstream_log_c() {
    // Verify descriptions match upstream rsync's log.c
    assert_eq!(ExitCode::Ok.description(), "success");
    assert_eq!(ExitCode::Syntax.description(), "syntax or usage error");
    assert_eq!(ExitCode::Protocol.description(), "protocol incompatibility");
    assert_eq!(
        ExitCode::FileSelect.description(),
        "errors selecting input/output files, dirs"
    );
    assert_eq!(
        ExitCode::Unsupported.description(),
        "requested action not supported"
    );
    assert_eq!(
        ExitCode::StartClient.description(),
        "error starting client-server protocol"
    );
    assert_eq!(ExitCode::SocketIo.description(), "error in socket IO");
    assert_eq!(ExitCode::FileIo.description(), "error in file IO");
    assert_eq!(
        ExitCode::StreamIo.description(),
        "error in rsync protocol data stream"
    );
    assert_eq!(
        ExitCode::MessageIo.description(),
        "errors with program diagnostics"
    );
    assert_eq!(ExitCode::Ipc.description(), "error in IPC code");
    assert_eq!(
        ExitCode::Crashed.description(),
        "received SIGSEGV or SIGBUS or SIGABRT"
    );
    assert_eq!(
        ExitCode::Terminated.description(),
        "received SIGINT, SIGTERM, or SIGHUP"
    );
    assert_eq!(ExitCode::Signal1.description(), "received SIGUSR1");
    assert_eq!(
        ExitCode::Signal.description(),
        "received SIGINT, SIGTERM, or SIGHUP"
    );
    assert_eq!(ExitCode::WaitChild.description(), "waitpid() failed");
    assert_eq!(
        ExitCode::Malloc.description(),
        "error allocating core memory buffers"
    );
    assert_eq!(ExitCode::PartialTransfer.description(), "partial transfer");
    assert_eq!(
        ExitCode::Vanished.description(),
        "some files vanished before they could be transferred"
    );
    assert_eq!(
        ExitCode::DeleteLimit.description(),
        "max delete limit stopped deletions"
    );
    assert_eq!(
        ExitCode::Timeout.description(),
        "timeout in data send/receive"
    );
    assert_eq!(
        ExitCode::ConnectionTimeout.description(),
        "timeout waiting for daemon connection"
    );
    assert_eq!(ExitCode::CommandFailed.description(), "remote command failed");
    assert_eq!(ExitCode::CommandKilled.description(), "remote command killed");
    assert_eq!(
        ExitCode::CommandRun.description(),
        "remote command could not be run"
    );
    assert_eq!(
        ExitCode::CommandNotFound.description(),
        "remote command not found"
    );
}

#[test]
fn exit_code_gaps_are_intentional() {
    // Verify that gaps in the exit code numbering are intentional
    // These ranges should NOT have exit codes defined
    let undefined_ranges = vec![
        (6, 9),    // Between StartClient and SocketIo
        (17, 18),  // Between Terminated and Signal1
        (26, 29),  // Between DeleteLimit and Timeout
        (31, 34),  // Between Timeout and ConnectionTimeout
        (36, 123), // Between ConnectionTimeout and CommandFailed
    ];

    for (start, end) in undefined_ranges {
        for value in start..=end {
            assert!(
                ExitCode::from_i32(value).is_none(),
                "Exit code {value} should not be defined (in gap range)"
            );
        }
    }
}
