// Test-only classification of the environment a SEND_ZC test runs in.
//
// Shared by the crate's unit tests and `tests/io_uring_send_zc.rs` (pulled in
// there with `#[path]`), so every SEND_ZC test states on stderr whether it
// exercised the real opcode or why it could not: io_uring unavailable, opcode
// unsupported, or a setup failure that RLIMIT_MEMLOCK explains. Reads
// `/proc/self` instead of calling libc so it needs no unsafe code.

use std::io;

/// Whether a SEND_ZC test can exercise the real opcode in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendZcEnv {
    /// io_uring works and the kernel advertises `IORING_OP_SEND_ZC`.
    Ok,
    /// `io_uring_setup(2)` is unavailable (old kernel, seccomp, sysctl).
    IoUringUnavailable,
    /// io_uring works but the kernel lacks the opcode (pre-6.0).
    OpcodeUnsupported,
}

/// Classifies from the two runtime probes the production code consults.
pub fn classify(io_uring_available: bool, opcode_supported: bool) -> SendZcEnv {
    match (io_uring_available, opcode_supported) {
        (false, _) => SendZcEnv::IoUringUnavailable,
        (true, false) => SendZcEnv::OpcodeUnsupported,
        (true, true) => SendZcEnv::Ok,
    }
}

/// Prints the classification for `test` and returns it.
pub fn report(test: &str, env: SendZcEnv) -> SendZcEnv {
    let verdict = match env {
        SendZcEnv::Ok => "ok - exercising IORING_OP_SEND_ZC",
        SendZcEnv::IoUringUnavailable => "io_uring unavailable - SEND_ZC not exercised",
        SendZcEnv::OpcodeUnsupported => {
            "SEND_ZC opcode unsupported (kernel < 6.0) - SEND_ZC not exercised"
        }
    };
    eprintln!("[send_zc env] {test}: {verdict} ({})", context());
    env
}

/// Prints a loud skip for a setup step that failed with `err`, naming
/// RLIMIT_MEMLOCK when it is finite and the error is the one it produces.
pub fn report_setup_failure(test: &str, step: &str, err: &io::Error) {
    let memlock_errno = matches!(err.raw_os_error(), Some(1 | 12)); // EPERM | ENOMEM
    let class = if memlock_errno && memlock_soft_limit().is_some_and(|l| l != "unlimited") {
        "memlock-limited"
    } else {
        "setup failed"
    };
    eprintln!(
        "[send_zc env] SKIP {test}: {class} - {step}: {err} ({})",
        context()
    );
}

fn context() -> String {
    format!(
        "euid {}, RLIMIT_MEMLOCK soft {}",
        effective_uid().unwrap_or_else(|| "unknown".to_owned()),
        memlock_soft_limit().unwrap_or_else(|| "unknown".to_owned())
    )
}

fn effective_uid() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2).map(str::to_owned)
}

fn memlock_soft_limit() -> Option<String> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max locked memory"))?;
    let soft = line["Max locked memory".len()..]
        .split_whitespace()
        .next()?;
    Some(if soft == "unlimited" {
        soft.to_owned()
    } else {
        format!("{soft} bytes")
    })
}
