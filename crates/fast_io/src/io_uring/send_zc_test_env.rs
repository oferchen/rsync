// Test-only classification of the environment a SEND_ZC test runs in.
//
// Shared by the crate's unit tests and `tests/io_uring_send_zc.rs` (pulled in
// there with `#[path]`), so every SEND_ZC test states on stderr whether it
// exercised the real opcode or why it could not: io_uring unavailable or the
// opcode unsupported. A ring the locked-memory budget refused is an asserted
// arm, not a skip. Reads `/proc` instead of calling libc so it needs no
// unsafe code.

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

/// Asserts that a setup step failed only because the kernel refused to
/// charge a ring to this user's locked-memory budget, and prints that the
/// refusal arm ran.
///
/// Some kernels charge a new ring's own pages to `RLIMIT_MEMLOCK` (Linux
/// v6.17 `io_uring/memmap.c` `io_create_region`), and the budget is per user,
/// shared by every process of that user, so under a parallel test run a ring
/// can be refused with `ENOMEM`. Production answers that by not using
/// io_uring; this is the only setup failure a test accepts. Any other error
/// fails the test, and so does an `ENOMEM` on a thread holding `CAP_IPC_LOCK`,
/// whose rings the kernel never charges (`io_uring/io_uring.c`
/// `io_uring_create`) - so the root CI job can only ever take the full arm.
pub fn assert_setup_refused_by_memlock(test: &str, step: &str, err: &io::Error) {
    assert_eq!(
        err.kind(),
        io::ErrorKind::OutOfMemory,
        "{test}: {step} failed for a reason other than the locked-memory budget: {err} ({})",
        context()
    );
    assert!(
        !thread_holds_ipc_lock(),
        "{test}: {step} returned ENOMEM on a thread holding CAP_IPC_LOCK, whose rings are \
         never charged: {err} ({})",
        context()
    );
    eprintln!(
        "[send_zc env] {test}: {step} refused with ENOMEM by the locked-memory budget - \
         asserted the refusal arm ({})",
        context()
    );
}

/// Asserts that a socket writer asked for SEND_ZC on a socket that accepts it
/// dropped SEND_ZC only because one of its rings was refused, and prints that
/// the refusal arm ran.
///
/// The socket-writer factory answers a ring it cannot build with the plain
/// writer, and the writer answers a SEND_ZC pipeline it cannot build with
/// plain sends; neither surfaces the error, so the test cannot read the errno.
/// A refusal is only possible without `CAP_IPC_LOCK` (rings are charged to the
/// user's shared budget only then), so a thread holding it fails here: the
/// root CI job always takes the full SEND_ZC arm. The caller still asserts
/// the payload arrived byte for byte.
pub fn assert_writer_ring_refused(test: &str) {
    assert!(
        !thread_holds_ipc_lock(),
        "{test}: the writer dropped SEND_ZC on a thread holding CAP_IPC_LOCK, whose rings \
         are never charged ({})",
        context()
    );
    eprintln!(
        "[send_zc env] {test}: the writer's ring was refused - asserting the plain-send \
         fallback delivers the payload ({})",
        context()
    );
}

/// Reads this thread's effective capability set from `/proc/thread-self` and
/// reports whether `CAP_IPC_LOCK` (bit 14) is in it.
fn thread_holds_ipc_lock() -> bool {
    const CAP_IPC_LOCK: u32 = 14;
    std::fs::read_to_string("/proc/thread-self/status")
        .ok()
        .and_then(|status| {
            let line = status.lines().find(|l| l.starts_with("CapEff:"))?;
            u64::from_str_radix(line["CapEff:".len()..].trim(), 16).ok()
        })
        .is_some_and(|caps| caps & (1 << CAP_IPC_LOCK) != 0)
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
