//! Linux platform tests for the splice module, split by concern.

mod recv_fd_to_file;
mod splice_to_file;
mod vmsplice;

/// Helper: creates a socketpair with a writer thread that sends `content`,
/// then closes the send end. Returns the recv fd.
pub(super) fn socketpair_with_writer(content: Vec<u8>) -> (i32, std::thread::JoinHandle<()>) {
    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    let handle = std::thread::spawn(move || {
        let mut offset = 0;
        while offset < content.len() {
            // SAFETY: the fd was opened just above and is still valid; the buffer
            // provides exactly the requested number of readable bytes.
            let n = unsafe {
                libc::write(
                    send_fd,
                    content[offset..].as_ptr().cast::<libc::c_void>(),
                    content.len() - offset,
                )
            };
            assert!(n > 0, "write to socket failed");
            offset += n as usize;
        }
        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(send_fd) };
    });

    (recv_fd, handle)
}
