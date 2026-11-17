//! Remote-shell trampoline that fixes which end of a dying server the client
//! meets first.
//!
//! Same argv model as `capture-rsh`: leading `-*` options and the host token
//! are dropped and the rest is spawned as the server command. Everything the
//! server writes is relayed to the client, and everything the client writes is
//! read the whole time: forwarded while the server takes it and discarded
//! after. So the client never blocks on a full socket and has no reason to
//! read early. Once the server has exited and its output is relayed,
//! `OC_ORDER_RSH_ORDER` decides what the client sees next:
//!
//! - `write-first`: the socket the client writes to is shut down for reading
//!   on this side, so the client's next write fails with `EPIPE`. What it had
//!   already queued is read to the end first (closing with bytes unread would
//!   reset the connection and fail the write with `ECONNRESET` instead), and
//!   the socket it reads from is held open until the client closes its end,
//!   so it never meets end of file before the failed write.
//! - `eof-first`: only the client's input is ended (the socket it reads from
//!   is shut down for writing from this side), and its writes keep being read
//!   and discarded, so no write fails and the client meets end of file when
//!   it next waits for input.
//!
//! The client must reach this program over two sockets, one per direction,
//! as rsync and oc-rsync connect a remote shell (upstream: pipe.c:57-72
//! piped_child() makes two util1.c fd_pair()s); a pipe cannot be half-closed
//! safely from here, so the program refuses it.

#[cfg(unix)]
fn main() {
    unix::run();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("order-rsh: Unix only");
    std::process::exit(1);
}

#[cfg(unix)]
mod unix {
    use std::env;
    use std::ffi::OsString;
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::process::{self, Command, Stdio};
    use std::thread;

    fn fail(what: &str, err: impl std::fmt::Display) -> ! {
        eprintln!("order-rsh: {what}: {err}");
        process::exit(1);
    }

    /// A second handle on `fd`, which must be a socket.
    fn socket_of(fd: &impl AsFd, what: &str) -> UnixStream {
        fd.as_fd()
            .try_clone_to_owned()
            .map(UnixStream::from)
            .unwrap_or_else(|e| fail(&format!("dup {what} (it must be a socket)"), e))
    }

    pub(super) fn run() {
        let eof_first = match env::var("OC_ORDER_RSH_ORDER").as_deref() {
            Ok("write-first") => false,
            Ok("eof-first") => true,
            other => fail("OC_ORDER_RSH_ORDER", format!("{other:?}")),
        };
        let argv: Vec<OsString> = env::args_os().skip(1).collect();
        let host = argv
            .iter()
            .position(|arg| !arg.to_string_lossy().starts_with('-'))
            .unwrap_or_else(|| fail("argv", "no host"));
        let command = argv
            .get(host + 1..)
            .filter(|rest| !rest.is_empty())
            .unwrap_or_else(|| fail("argv", "no remote command"));

        let mut server = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| fail("spawn server", e));
        let mut server_in = server.stdin.take().expect("piped stdin");
        let mut server_out = server.stdout.take().expect("piped stdout");

        // The client's writes are read for as long as the client writes:
        // forwarded while the server takes them, discarded once it is gone.
        let from_client = thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = vec![0u8; 65536];
            let mut forwarding = true;
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if forwarding && server_in.write_all(&buf[..n]).is_err() {
                    forwarding = false;
                }
            }
        });

        let mut stdout = io::stdout().lock();
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match server_out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => fail("read server", e),
            };
            if let Err(e) = stdout.write_all(&buf[..n]).and_then(|()| stdout.flush()) {
                fail("write client", e);
            }
        }
        server.wait().unwrap_or_else(|e| fail("wait server", e));

        if eof_first {
            socket_of(&stdout, "the client's input")
                .shutdown(Shutdown::Write)
                .unwrap_or_else(|e| fail("shut down the client's input", e));
            // Keep reading until the client is done writing, so none of its
            // writes can fail.
            let _ = from_client.join();
            return;
        }
        socket_of(&io::stdin(), "the client's output")
            .shutdown(Shutdown::Read)
            .unwrap_or_else(|e| fail("shut down the client's output", e));
        // No write can be queued after the shutdown, so the reader drains
        // what was queued before it and then meets end of file.
        let _ = from_client.join();
        // The client's input stays open until the client closes its end:
        // its next write fails before it can see end of file.
        let _ = socket_of(&stdout, "the client's input").read(&mut [0u8; 1]);
    }
}
