//! Remote-shell trampoline that stalls one direction of the wire.
//!
//! Same argv model as `capture-rsh`: leading `-*` options and the host token
//! are dropped and the rest is spawned as the server command. Both directions
//! are relayed until the one named by `OC_STALL_RSH_DIR` has forwarded
//! `STALL_AFTER` bytes; that direction is then never read again:
//!
//! - `in` (push): the client's input is no longer read, and `stalled <pid>`
//!   names the client (this process's parent). Signalling it is the
//!   caller's job: only an ancestor of the client may read the client's
//!   `/proc/<tid>/syscall` under Yama, and that is the exact test for a
//!   blocked write. A full socket is not: the kernel's flow control for
//!   `AF_UNIX` streams is not exposed in a stable form. The stall is kept
//!   until the client has exited, so it cannot end on end of file.
//! - `out` (pull): the server's stdout pipe is no longer drained. Once the
//!   pipe polls not-writable and a server thread is parked in `poll`, the
//!   server receives `OC_STALL_RSH_SIGNAL` (`INT`, `TERM` or `HUP`). A full
//!   pipe alone is not enough: a server still between its last write and its
//!   wait would see the signal at a check in user space, not in the wait under
//!   test. The stalled pipe stays open until the server has exited, so the
//!   server cannot end on `EPIPE` instead.
//!
//! Progress goes to the file `OC_STALL_RSH_STATUS`, one line each:
//! `stalled <pid>` or `signalled`, then `server_exit <code>`, or
//! `error: <reason>` when the shim itself fails.
//!
//! Linux only: it reads `/proc/<tid>/syscall` and waits on a pidfd.

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("stall-rsh: Linux only (it reads /proc/<tid>/syscall)");
    std::process::exit(1);
}

/// Bytes relayed in the stalled direction before it stops; enough to be past
/// the protocol handshake and into file data.
#[cfg(target_os = "linux")]
const STALL_AFTER: usize = 200_000;

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::ffi::OsString;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::AsFd;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::process::{Pid, PidfdFlags, Signal, kill_process, pidfd_open};

    use super::STALL_AFTER;

    struct Status(OsString);

    impl Status {
        fn note(&self, line: &str) {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.0)
                .expect("open status file");
            writeln!(file, "{line}").expect("write status file");
        }

        fn fail(&self, what: &str, err: impl std::fmt::Display) -> ! {
            self.note(&format!("error: {what}: {err}"));
            std::process::exit(1);
        }
    }

    fn env_var(name: &str) -> String {
        env::var(name).unwrap_or_else(|_| {
            eprintln!("stall-rsh: {name} is not set");
            std::process::exit(1);
        })
    }

    /// Copies `src` to `dst` until end of file or, with a limit, until that
    /// many bytes have passed; hands `src` back so it stays open.
    fn relay<R: Read, W: Write>(mut src: R, mut dst: W, limit: Option<usize>) -> R {
        let mut buf = vec![0u8; 65536];
        let mut total = 0;
        while limit.is_none_or(|limit| total < limit) {
            let n = match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if dst.write_all(&buf[..n]).and_then(|()| dst.flush()).is_err() {
                break;
            }
            total += n;
        }
        src
    }

    pub(super) fn run() {
        let status = Status(OsString::from(env_var("OC_STALL_RSH_STATUS")));
        let stall_in = match env_var("OC_STALL_RSH_DIR").as_str() {
            "in" => true,
            "out" => false,
            other => status.fail("OC_STALL_RSH_DIR", other),
        };

        let argv: Vec<OsString> = env::args_os().skip(1).collect();
        let host = argv
            .iter()
            .position(|arg| !arg.to_string_lossy().starts_with('-'))
            .unwrap_or_else(|| status.fail("argv", "no host"));
        let command = argv
            .get(host + 1..)
            .filter(|rest| !rest.is_empty())
            .unwrap_or_else(|| status.fail("argv", "no remote command"));

        let (out_r, out_w) = io::pipe().unwrap_or_else(|e| status.fail("pipe", e));
        let probe_w = out_w.try_clone().unwrap_or_else(|e| status.fail("dup", e));
        let mut server = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(out_w)
            .spawn()
            .unwrap_or_else(|e| status.fail("spawn server", e));
        let server_in = server.stdin.take().expect("piped stdin");

        let (in_limit, out_limit) = if stall_in {
            (Some(STALL_AFTER), None)
        } else {
            (None, Some(STALL_AFTER))
        };
        let pump_in = thread::spawn(move || {
            let mut server_in = server_in;
            let _stdin = relay(io::stdin().lock(), &mut server_in, in_limit);
            server_in
        });
        let pump_out = thread::spawn(move || relay(out_r, io::stdout().lock(), out_limit));

        // The stalled direction stays open until the server has exited:
        // closing it would end the server through EPIPE instead.
        let mut stalled_output = None;
        if stall_in {
            let server_in = pump_in.join().expect("input pump");
            let client = std::os::unix::process::parent_id();
            let client_pid = i32::try_from(client).ok().and_then(Pid::from_raw);
            let client_pid = client_pid.unwrap_or_else(|| status.fail("client pid", client));
            let client_pidfd = pidfd_open(client_pid, PidfdFlags::empty())
                .unwrap_or_else(|e| status.fail("pidfd", e));
            status.note(&format!("stalled {client}"));
            // Keep the stall until the client has exited on its own:
            // releasing the server first would end the client through end of
            // file instead.
            wait_readable(&client_pidfd, &status);
            drop(server_in);
        } else {
            stalled_output = Some(pump_out.join().expect("output pump"));
            let signal = match env_var("OC_STALL_RSH_SIGNAL").as_str() {
                "INT" => Signal::INT,
                "TERM" => Signal::TERM,
                "HUP" => Signal::HUP,
                other => status.fail("OC_STALL_RSH_SIGNAL", other),
            };
            let poll_number = poll_syscall_number(&status);
            while pipe_writable(&probe_w, &status) || !parked_in(server.id(), &poll_number, &status)
            {
                thread::yield_now();
            }
            let pid = i32::try_from(server.id()).ok().and_then(Pid::from_raw);
            let pid = pid.unwrap_or_else(|| status.fail("server pid", server.id()));
            status.note("signalled");
            kill_process(pid, signal).unwrap_or_else(|e| status.fail("kill server", e));
        }
        let exit = server
            .wait()
            .unwrap_or_else(|e| status.fail("wait server", e));
        status.note(&format!("server_exit {}", exit.code().unwrap_or(-1)));
        drop(stalled_output);
    }

    /// The number `/proc/<tid>/syscall` shows for a thread parked in the
    /// rustix `poll` oc's transport waits in. It is learned from a thread of
    /// ours parked in that same call, so no per-architecture table is needed.
    fn poll_syscall_number(status: &Status) -> String {
        let (idle_r, idle_w) = io::pipe().unwrap_or_else(|e| status.fail("pipe", e));
        let (tx, rx) = mpsc::channel();
        let parked = thread::spawn(move || {
            let _ = tx.send(fs::read_link("/proc/thread-self"));
            let mut fds = [PollFd::new(&idle_r, PollFlags::IN)];
            let _ = poll(&mut fds, None);
        });
        let task = rx
            .recv()
            .expect("calibration thread")
            .unwrap_or_else(|e| status.fail("/proc/thread-self", e));
        let task = Path::new("/proc").join(task);
        let number = loop {
            if let Some(number) = blocked_syscall(&task, status) {
                break number;
            }
            thread::yield_now();
        };
        drop(idle_w);
        parked.join().expect("calibration thread");
        number
    }

    /// Whether some thread of `pid` sleeps in the syscall `number`. Reading a
    /// child's `syscall` file needs ptrace access, which its parent has.
    fn parked_in(pid: u32, number: &str, status: &Status) -> bool {
        let tasks = fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap_or_else(|e| status.fail("server tasks", e));
        tasks
            .flatten()
            .any(|task| blocked_syscall(&task.path(), status).as_deref() == Some(number))
    }

    /// The syscall a sleeping thread is blocked in, or `None` while it runs
    /// or once it has exited.
    fn blocked_syscall(task: &Path, status: &Status) -> Option<String> {
        let stat = fs::read_to_string(task.join("stat")).ok()?;
        let state = stat.rsplit_once(')')?.1.split_whitespace().next()?;
        if state != "S" {
            return None;
        }
        let syscall = match fs::read_to_string(task.join("syscall")) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => status.fail(&format!("read {}/syscall", task.display()), e),
        };
        let number = syscall.split_whitespace().next()?;
        (number != "running").then(|| number.to_owned())
    }

    fn pipe_writable(fd: impl AsFd, status: &Status) -> bool {
        let mut fds = [PollFd::new(&fd, PollFlags::OUT)];
        poll(&mut fds, Some(&Timespec::default())).unwrap_or_else(|e| status.fail("poll", e));
        fds[0].revents().contains(PollFlags::OUT)
    }

    fn wait_readable(fd: impl AsFd, status: &Status) {
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        loop {
            match poll(&mut fds, None) {
                Ok(_) => return,
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => status.fail("poll pidfd", e),
            }
        }
    }
}
