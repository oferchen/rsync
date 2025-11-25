use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::process::Child;
use std::sync::mpsc::Sender;
use std::thread;

use tempfile::NamedTempFile;

use crate::{
    client::{ClientError, HumanReadableMode},
    message::Role,
    rsync_error,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FallbackStreamKind {
    Stdout,
    Stderr,
}

pub(crate) enum FallbackStreamMessage {
    Data(FallbackStreamKind, Vec<u8>),
    Error(FallbackStreamKind, io::Error),
    Finished(FallbackStreamKind),
}

#[track_caller]
pub(crate) fn fallback_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(1, "{}", text.into()).with_role(Role::Client);
    ClientError::new(1, message)
}

pub(crate) fn push_toggle(
    args: &mut Vec<OsString>,
    enable: &str,
    disable: &str,
    setting: Option<bool>,
) {
    match setting {
        Some(true) => args.push(OsString::from(enable)),
        Some(false) => args.push(OsString::from(disable)),
        None => {}
    }
}

pub(crate) fn push_human_readable(args: &mut Vec<OsString>, mode: Option<HumanReadableMode>) {
    match mode {
        Some(HumanReadableMode::Disabled) => args.push(OsString::from("--no-human-readable")),
        Some(HumanReadableMode::Enabled) => args.push(OsString::from("--human-readable")),
        Some(HumanReadableMode::Combined) => args.push(OsString::from("--human-readable=2")),
        None => {}
    }
}

pub(crate) fn prepare_file_list(
    entries: &[OsString],
    files_from_used: bool,
    zero_terminated: bool,
) -> io::Result<Option<NamedTempFile>> {
    if !files_from_used {
        return Ok(None);
    }

    let mut file = NamedTempFile::new()?;
    {
        let writer = file.as_file_mut();
        for entry in entries {
            write_file_list_entry(writer, entry.as_os_str())?;
            if zero_terminated {
                writer.write_all(&[0])?;
            } else {
                writer.write_all(b"\n")?;
            }
        }
        writer.flush()?;
    }

    Ok(Some(file))
}

fn write_file_list_entry<W: Write>(writer: &mut W, value: &OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        writer.write_all(value.as_bytes())
    }

    #[cfg(not(unix))]
    {
        writer.write_all(value.to_string_lossy().as_bytes())
    }
}

pub(crate) fn spawn_fallback_reader<R>(
    mut reader: R,
    kind: FallbackStreamKind,
    sender: Sender<FallbackStreamMessage>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(FallbackStreamMessage::Finished(kind));
                    break;
                }
                Ok(n) => {
                    if sender
                        .send(FallbackStreamMessage::Data(kind, Vec::from(&buffer[..n])))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(FallbackStreamMessage::Error(kind, error));
                    break;
                }
            }
        }
    })
}

/// Writes the daemon password into `writer`, appending a newline when required and
/// scrubbing the buffer afterwards.
pub(crate) fn write_daemon_password<W: Write>(
    writer: &mut W,
    password: &mut Vec<u8>,
) -> io::Result<()> {
    if !password.ends_with(b"\n") {
        password.push(b'\n');
    }

    writer.write_all(password)?;
    writer.flush()?;

    for byte in password.iter_mut() {
        *byte = 0;
    }

    Ok(())
}

pub(crate) fn join_fallback_thread(handle: &mut Option<thread::JoinHandle<()>>) {
    if let Some(join_handle) = handle.take() {
        let _ = join_handle.join();
    }
}

pub(crate) fn terminate_fallback_process(
    child: &mut Child,
    stdout_thread: &mut Option<thread::JoinHandle<()>>,
    stderr_thread: &mut Option<thread::JoinHandle<()>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    join_fallback_thread(stdout_thread);
    join_fallback_thread(stderr_thread);
}
