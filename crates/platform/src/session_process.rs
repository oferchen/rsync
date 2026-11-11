//! Per-connection session processes for the Windows daemon.
//!
//! Upstream forks a child for every accepted connection and the child serves
//! the session after closing the listening sockets (socket.c:755-780). Windows
//! has no `fork`, so the daemon starts a fresh `oc-rsync` process for each
//! connection instead, and hands it the accepted socket.
//!
//! The hand-off keeps upstream's isolation:
//! - The child is created suspended, so nothing runs until the parent has
//!   duplicated the socket for it and written the hand-off record.
//! - Only the handles named in the spawn call are inherited
//!   (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). The listening sockets and every
//!   other handle the daemon holds stay out of the child, as upstream closes
//!   them in the child at socket.c:764-767.
//! - The socket itself is never inherited: [`duplicate_socket`](crate::session_process::duplicate_socket) produces a
//!   `WSAPROTOCOL_INFOW` record valid only for the target process, and the
//!   child rebuilds the socket from it with [`adopt_socket`](crate::session_process::adopt_socket).
//!
//! This module owns the Win32 calls so that `crates/daemon`, which is
//! `#![deny(unsafe_code)]`, can split connections through a safe API.
#![cfg(windows)]
#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::net::TcpStream;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{
    AsRawHandle, AsRawSocket, BorrowedHandle, FromRawHandle, FromRawSocket, OwnedHandle, RawHandle,
};
use std::path::Path;
use std::sync::Once;

use windows::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Networking::WinSock::{
    FROM_PROTOCOL_INFO, SOCKET, WSA_FLAG_OVERLAPPED, WSADATA, WSADuplicateSocketW, WSAGetLastError,
    WSAPROTOCOL_INFOW, WSASocketW, WSAStartup,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// Size of the duplicated-socket record [`duplicate_socket`] produces.
pub const SOCKET_INFO_LEN: usize = size_of::<WSAPROTOCOL_INFOW>();

/// Exit code given to a suspended child that is dropped before it resumes.
const ABANDONED_EXIT_CODE: u32 = 1;

/// The three standard handles a session child starts with.
pub struct ChildStdio<'a> {
    /// Read end of the hand-off pipe.
    pub stdin: BorrowedHandle<'a>,
    /// Where the child's standard output goes.
    pub stdout: BorrowedHandle<'a>,
    /// Where the child's standard error goes.
    pub stderr: BorrowedHandle<'a>,
}

/// A child process created suspended: it has not run a single instruction.
///
/// Dropping it without [`resume`](Self::resume) terminates the process, so a
/// hand-off that fails half way never leaves a half-initialised session.
pub struct SuspendedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

/// A running session child, owned by the daemon until it is reaped.
pub struct SessionChild {
    process: OwnedHandle,
    pid: u32,
}

impl SuspendedChild {
    /// Process id, needed to duplicate a socket for this child.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Lets the child run.
    pub fn resume(self) -> io::Result<SessionChild> {
        // SAFETY: `self.thread` is the primary thread handle CreateProcessW
        // returned for this child, still open because `self` owns it.
        let previous = unsafe { ResumeThread(HANDLE(self.thread.as_raw_handle())) };
        if previous == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is never dropped, so each handle is read out exactly
        // once and ownership moves to the returned value or is closed here.
        let (process, thread) =
            unsafe { (std::ptr::read(&this.process), std::ptr::read(&this.thread)) };
        drop(thread);
        Ok(SessionChild {
            process,
            pid: this.pid,
        })
    }
}

impl Drop for SuspendedChild {
    fn drop(&mut self) {
        // SAFETY: `self.process` is this child's process handle, open while
        // `self` lives. A failure means the process is already gone.
        let _ =
            unsafe { TerminateProcess(HANDLE(self.process.as_raw_handle()), ABANDONED_EXIT_CODE) };
    }
}

impl SessionChild {
    /// Process id of the session child.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the exit code if the child has exited, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<u32>> {
        self.wait_for(0)
    }

    /// Blocks until the child exits and returns its exit code.
    pub fn wait(&mut self) -> io::Result<u32> {
        self.wait_for(INFINITE)?
            .ok_or_else(|| io::Error::other("infinite wait returned before exit"))
    }

    fn wait_for(&mut self, millis: u32) -> io::Result<Option<u32>> {
        let handle = HANDLE(self.process.as_raw_handle());
        // SAFETY: `handle` is this child's process handle, open while `self`
        // lives; waiting on it has no other effect.
        let state = unsafe { WaitForSingleObject(handle, millis) };
        if state == WAIT_TIMEOUT {
            return Ok(None);
        }
        if state != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: the process has exited, `handle` is still open and `code`
        // is a valid out-pointer.
        unsafe { GetExitCodeProcess(handle, &mut code) }.map_err(io::Error::from)?;
        Ok(Some(code))
    }
}

/// Starts `program --daemon-session` suspended, inheriting only `stdio`.
///
/// The command line names nothing else: the hand-off record carries the
/// session's inputs over `stdio.stdin`, so no daemon argument is re-quoted.
pub fn spawn_suspended(program: &Path, stdio: &ChildStdio<'_>) -> io::Result<SuspendedChild> {
    let inherited = [stdio.stdin, stdio.stdout, stdio.stderr];
    for handle in &inherited {
        set_inheritable(*handle)?;
    }
    let mut handle_list: Vec<HANDLE> = inherited
        .iter()
        .map(|h| HANDLE(h.as_raw_handle()))
        .collect();
    handle_list.dedup_by_key(|h| h.0);

    let attributes = AttributeList::with_handle_list(&mut handle_list)?;
    // SAFETY: STARTUPINFOEXW is a plain C struct; all-zero is its documented
    // initial state before `cb` and the used fields are set.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = HANDLE(stdio.stdin.as_raw_handle());
    startup.StartupInfo.hStdOutput = HANDLE(stdio.stdout.as_raw_handle());
    startup.StartupInfo.hStdError = HANDLE(stdio.stderr.as_raw_handle());
    startup.lpAttributeList = attributes.as_ptr();

    let program_wide = wide(program.as_os_str());
    let mut command_line = wide(&command_line_for(program));
    // SAFETY: PROCESS_INFORMATION is a plain C out-struct.
    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
    // SAFETY: every pointer is valid for the call: the program and command
    // line are NUL-terminated buffers owned here (the command line is
    // mutable as CreateProcessW requires), `startup` embeds an attribute list
    // that lives in `attributes` until after the call, and `info` is an
    // out-pointer. Inheritance is on but limited to `handle_list`, which holds
    // only the three handles made inheritable above.
    unsafe {
        CreateProcessW(
            PCWSTR(program_wide.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .map_err(io::Error::from)?;
    drop(attributes);
    // SAFETY: CreateProcessW succeeded, so both handles are fresh and owned
    // by this call; nothing else closes them.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0 as RawHandle),
            OwnedHandle::from_raw_handle(info.hThread.0 as RawHandle),
        )
    };
    Ok(SuspendedChild {
        process,
        thread,
        pid: info.dwProcessId,
    })
}

/// The command line a session child starts with: the quoted program and the
/// internal session flag. Paths cannot contain `"`, so quoting is exact.
fn command_line_for(program: &Path) -> std::ffi::OsString {
    let mut line = std::ffi::OsString::from("\"");
    line.push(program.as_os_str());
    line.push("\" --daemon-session");
    line
}

/// Describes `socket` for process `pid`, which can rebuild it with
/// [`adopt_socket`]. The record is useless to any other process.
pub fn duplicate_socket(socket: &TcpStream, pid: u32) -> io::Result<Vec<u8>> {
    // SAFETY: WSAPROTOCOL_INFOW is a plain C out-struct.
    let mut info: WSAPROTOCOL_INFOW = unsafe { zeroed() };
    // SAFETY: the raw socket is valid while `socket` is borrowed, and `info`
    // is a valid out-pointer of the documented type.
    let status =
        unsafe { WSADuplicateSocketW(SOCKET(socket.as_raw_socket() as usize), pid, &mut info) };
    if status != 0 {
        return Err(wsa_error());
    }
    // SAFETY: `info` is fully initialised by the successful call and viewed
    // as its own bytes, which stay valid for the copy.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&info as *const WSAPROTOCOL_INFOW).cast::<u8>(),
            SOCKET_INFO_LEN,
        )
    };
    Ok(bytes.to_vec())
}

/// Rebuilds the socket a parent duplicated for this process.
pub fn adopt_socket(info: &[u8]) -> io::Result<TcpStream> {
    if info.len() != SOCKET_INFO_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "duplicated socket record is {} bytes, expected {SOCKET_INFO_LEN}",
                info.len()
            ),
        ));
    }
    winsock_startup()?;
    // SAFETY: WSAPROTOCOL_INFOW is plain old data, `info` has exactly its size,
    // and read_unaligned tolerates any alignment of the byte slice.
    let protocol: WSAPROTOCOL_INFOW =
        unsafe { std::ptr::read_unaligned(info.as_ptr().cast::<WSAPROTOCOL_INFOW>()) };
    // SAFETY: `protocol` lives across the call; FROM_PROTOCOL_INFO tells
    // WinSock to take family, type and protocol from it.
    let socket = unsafe {
        WSASocketW(
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            FROM_PROTOCOL_INFO,
            Some(&protocol),
            0,
            WSA_FLAG_OVERLAPPED,
        )
    }
    .map_err(|_| wsa_error())?;
    // SAFETY: WSASocketW returned a fresh socket owned by nothing else, so
    // the TcpStream takes sole ownership and closes it on drop.
    Ok(unsafe { TcpStream::from_raw_socket(socket.0 as u64) })
}

/// Takes ownership of a standard handle the parent passed to this process.
///
/// Refuses anything but a pipe, so a session child started by hand with an
/// unrelated standard input fails closed instead of reading from it.
pub fn adopt_handoff_pipe(handle: OwnedHandle) -> io::Result<std::fs::File> {
    use windows::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
    // SAFETY: `handle` is open while owned here; GetFileType only queries it.
    let kind = unsafe { GetFileType(HANDLE(handle.as_raw_handle())) };
    if kind != FILE_TYPE_PIPE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the session hand-off must arrive on a pipe",
        ));
    }
    Ok(std::fs::File::from(handle))
}

fn set_inheritable(handle: BorrowedHandle<'_>) -> io::Result<()> {
    // SAFETY: `handle` is a valid open handle for the borrow's lifetime, and
    // only its inherit flag changes.
    unsafe {
        SetHandleInformation(
            HANDLE(handle.as_raw_handle()),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS(HANDLE_FLAG_INHERIT.0),
        )
    }
    .map_err(io::Error::from)
}

fn winsock_startup() -> io::Result<()> {
    static START: Once = Once::new();
    let mut status = 0;
    START.call_once(|| {
        // SAFETY: WSADATA is a plain C out-struct; 2.2 is the version std and
        // every supported Windows provide. The reference is never released:
        // the process needs WinSock until it exits.
        let mut data: WSADATA = unsafe { zeroed() };
        status = unsafe { WSAStartup(0x0202, &mut data) };
    });
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    Ok(())
}

fn wsa_error() -> io::Error {
    // SAFETY: WSAGetLastError only reads this thread's last WinSock error.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0)
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// An initialised `PROC_THREAD_ATTRIBUTE_LIST` holding one handle list.
struct AttributeList {
    buffer: Vec<u8>,
}

impl AttributeList {
    fn with_handle_list(handles: &mut [HANDLE]) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: the sizing call takes a null list and only writes `size`;
        // it reports ERROR_INSUFFICIENT_BUFFER by design, so its result is
        // ignored and `size` checked instead.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        // SAFETY: `buffer` has the size the sizing call asked for and outlives
        // the list, which is deleted in Drop before the buffer is freed.
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
            .map_err(io::Error::from)?;
        let this = Self { buffer };
        // SAFETY: the list was initialised above; `handles` outlives every use
        // of the list (the caller keeps it alive across CreateProcessW), and
        // the size passed is exactly the array's byte length.
        unsafe {
            UpdateProcThreadAttribute(
                this.as_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(this)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_ptr() as *mut _)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialised in `with_handle_list` and is
        // deleted exactly once, before its buffer is freed.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_quotes_the_program_and_adds_only_the_session_flag() {
        let line = command_line_for(Path::new(r"C:\Program Files\oc-rsync\oc-rsync.exe"));
        assert_eq!(
            line,
            r#""C:\Program Files\oc-rsync\oc-rsync.exe" --daemon-session"#
        );
    }

    #[test]
    fn a_socket_record_of_the_wrong_size_is_refused() {
        let error = adopt_socket(&[0u8; 8]).expect_err("short record");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    /// The parent duplicates a connected socket for this very process and
    /// adopts it back: bytes written through the adopted copy reach the peer.
    #[test]
    fn a_duplicated_socket_carries_the_connection() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (accepted, _) = listener.accept().expect("accept");
        let record = duplicate_socket(&accepted, std::process::id()).expect("duplicate");
        assert_eq!(record.len(), SOCKET_INFO_LEN);
        let mut adopted = adopt_socket(&record).expect("adopt");
        drop(accepted);
        adopted.write_all(b"ping").expect("write");
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn a_file_is_not_accepted_as_the_handoff_pipe() {
        let file = tempfile::tempfile().expect("tempfile");
        let error = adopt_handoff_pipe(OwnedHandle::from(file)).expect_err("not a pipe");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_pipe_is_accepted_as_the_handoff_pipe() {
        let (reader, _writer) = std::io::pipe().expect("pipe");
        adopt_handoff_pipe(OwnedHandle::from(reader)).expect("pipe accepted");
    }
}
