//! Remote-shell trampoline that shrinks its end of the client's connection.
//!
//! rsync runs its remote shell with the connection to the server on standard
//! input and output. This drops the host token, sets the send and receive
//! buffers of both descriptors to the smallest the kernel allows and then
//! becomes the server command, so a test can run a transfer whose server
//! end holds next to nothing in flight. A descriptor that is not a socket is
//! an error: a test that asked for small buffers must not run with large ones.

#[cfg(unix)]
fn main() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, exit};

    let mut args = std::env::args_os().skip(2);
    let Some(program) = args.next() else {
        eprintln!("small-buffer-rsh: no server command");
        exit(1);
    };
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    for (name, shrunk) in [
        ("stdin", shrink(&socket2::SockRef::from(&stdin))),
        ("stdout", shrink(&socket2::SockRef::from(&stdout))),
    ] {
        if let Err(error) = shrunk {
            eprintln!("small-buffer-rsh: {name}: {error}");
            exit(1);
        }
    }
    let error = Command::new(program).args(args).exec();
    eprintln!("small-buffer-rsh: exec: {error}");
    exit(127);
}

/// Asks for one-byte buffers; the kernel rounds the request up to its minimum.
#[cfg(unix)]
fn shrink(socket: &socket2::SockRef<'_>) -> std::io::Result<()> {
    socket.set_send_buffer_size(1)?;
    socket.set_recv_buffer_size(1)
}

#[cfg(not(unix))]
fn main() {
    eprintln!("small-buffer-rsh: only Unix remote shells pass the connection on stdio");
    std::process::exit(1);
}
