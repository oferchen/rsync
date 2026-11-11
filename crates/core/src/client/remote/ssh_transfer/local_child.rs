//! Local copy as a sender plus a server receiver.
//!
//! Upstream runs every local copy as a sender parent plus a forked `--server`
//! child that receives (main.c:666-667 `do_cmd()` -> pipe.c:109-160
//! `local_child()`). oc-rsync runs the child on a thread of its own, in a log
//! session of its own, over two pipes, so each role runs the code a remote
//! transfer runs.

use std::io::{PipeReader, PipeWriter, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use engine::batch::BatchWriter;

use super::super::super::config::ClientConfig;
use super::super::super::error::{ClientError, IPC_EXIT_CODE, invalid_argument_error};
use super::super::super::progress::ClientProgressObserver;
use super::super::super::summary::ClientSummary;
use super::drive::run_push_transfer;
use super::exit_status::{PeerChild, PeerConnection, PeerExit};
use super::server_config::build_server_config_for_receiver;
use crate::server::{ForkPoint, ServerConfig, ServerRole};

/// Runs a local copy as a push to a server receiver on its own thread.
///
/// The calling thread is the sender, as upstream asserts (pipe.c:116); the
/// receiver reads and writes its own ends of two pipes, as upstream's child
/// reads and writes the `fd_pair()` ends dup'ed onto its stdio
/// (pipe.c:119-160).
///
/// # Errors
///
/// Returns an IPC error (exit 14) when the pipes or the receiver thread cannot
/// be created, and otherwise any error the push transfer reports.
pub fn run_local_child_transfer(
    mut config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    config.apply_local_child_defaults();
    let Some((destination, sources)) = config.transfer_args().split_last() else {
        return Err(invalid_argument_error(
            "a local copy needs a source and a destination",
            1,
        ));
    };
    let receiver = local_receiver_config(&config, destination)?;
    let child = LocalChild::start(receiver)?;
    let sender = config.local_sender().unwrap_or(&config);
    run_push_transfer(sender, child, sources, observer, batch_writer)
}

/// Builds the receiver's configuration from the option set the forked child
/// inherits.
///
/// upstream: pipe.c:131-141 - the child becomes a server receiver and drops
/// what its sending side handles: `--files-from`, `--munge-links`, `--chmod`
/// and the log file. Its `-M` values are already part of `config`
/// (pipe.c:143-148).
fn local_receiver_config(
    config: &ClientConfig,
    destination: &std::ffi::OsStr,
) -> Result<ServerConfig, ClientError> {
    let mut receiver = build_server_config_for_receiver(config, &[destination.to_os_string()])?;
    receiver.chmod = None;
    Ok(receiver)
}

/// The receiver half of a local copy and the sender's ends of its pipes.
struct LocalChild {
    reader: PipeReader,
    writer: PipeWriter,
    receiver: LocalReceiver,
}

impl LocalChild {
    /// Creates both pipes and starts the receiver on its own thread.
    ///
    /// upstream: pipe.c:118-127 - `fd_pair()` twice, then `do_fork()`, each
    /// failure exiting with RERR_IPC.
    fn start(config: ServerConfig) -> Result<Self, ClientError> {
        let ipc = |op: &str, e: std::io::Error| {
            invalid_argument_error(&format!("{op}: {e}"), IPC_EXIT_CODE)
        };
        let (child_in, writer) = std::io::pipe().map_err(|e| ipc("pipe", e))?;
        let (reader, child_out) = std::io::pipe().map_err(|e| ipc("pipe", e))?;
        let session = logging::current().fork();
        let thread = std::thread::Builder::new()
            .name("receiver".to_owned())
            .spawn(move || {
                let _session = logging::enter(session);
                run_receiver(config, child_in, child_out)
            })
            .map_err(|e| ipc("fork", e))?;
        Ok(Self {
            reader,
            writer,
            receiver: LocalReceiver(thread),
        })
    }
}

impl PeerConnection for LocalChild {
    type Reader = PipeReader;
    type Writer = PipeWriter;
    type Child = LocalReceiver;

    fn split(self) -> std::io::Result<(PipeReader, PipeWriter, LocalReceiver)> {
        Ok((self.reader, self.writer, self.receiver))
    }
}

/// The receiver thread, joined as upstream's parent reaps its child.
struct LocalReceiver(JoinHandle<i32>);

impl PeerChild for LocalReceiver {
    fn cancel_connect_watchdog(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn reap(self) -> PeerExit {
        self.0.join().map_or(PeerExit::CRASHED, PeerExit::exited)
    }
}

/// Runs the server receiver over its pipe ends and returns the status the
/// child would exit with.
///
/// The receiver confines its thread first, then runs on a worker pool it
/// builds afterwards, so every thread that does receiver work inherits the
/// confinement as every thread of upstream's forked child does. A pool built
/// before the confinement, like the process-wide one, would escape it.
///
/// upstream: pipe.c:159 `child_main(argc, argv)` is `start_server()`, which
/// ends in `exit_cleanup()` (main.c:1336).
fn run_receiver(config: ServerConfig, input: PipeReader, output: PipeWriter) -> i32 {
    crate::session::confine_receiver(&config);
    run_confined(|| serve_receiver(config, input, output)).unwrap_or_else(|e| {
        let _ = writeln!(std::io::stderr(), "fork: {e}");
        IPC_EXIT_CODE
    })
}

/// Runs `work` on a worker pool built from the calling, already confined,
/// thread, so every pool thread inherits the confinement.
fn run_confined<T: Send>(
    work: impl FnOnce() -> T + Send,
) -> Result<T, rayon::ThreadPoolBuildError> {
    Ok(receiver_pool()?.install(work))
}

fn receiver_pool() -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    let session = logging::Session::capture();
    rayon::ThreadPoolBuilder::new()
        .thread_name(|index| format!("receiver-{index}"))
        .start_handler(move |_| {
            // The guard lives as long as the worker: each pool thread logs
            // into the receiver's session for its whole life.
            std::mem::forget(session.enter());
        })
        .build()
}

fn serve_receiver(config: ServerConfig, input: PipeReader, output: PipeWriter) -> i32 {
    let fork_point = ForkPoint::default();
    #[cfg(unix)]
    let outcome = crate::session::run_server_fds(
        config,
        std::fs::File::from(std::os::fd::OwnedFd::from(input)),
        std::fs::File::from(std::os::fd::OwnedFd::from(output)),
        None,
        None,
    );
    #[cfg(not(unix))]
    let outcome = {
        let (mut input, mut output) = (input, output);
        crate::session::run_server_stdio(config, &mut input, &mut output, None)
    };
    crate::session::server_exit_status(
        outcome,
        ServerRole::Receiver,
        &fork_point,
        crate::branding::detect_brand(None),
        &mut std::io::stderr(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: a receiver that panics must surface as a crashed child, never as a
    // clean exit that would let the sender report success.
    #[test]
    fn a_panicking_receiver_reaps_as_a_crash() {
        let thread = std::thread::spawn(|| -> i32 { panic!("receiver died") });
        assert_eq!(LocalReceiver(thread).reap(), PeerExit::CRASHED);
    }

    // WHY: the receiver's exit status is what the client folds into its own
    // exit code, exactly as a reaped child's WEXITSTATUS.
    #[test]
    fn a_receiver_exit_code_reaps_as_the_childs_status() {
        let thread = std::thread::spawn(|| 23);
        assert_eq!(LocalReceiver(thread).reap(), PeerExit::exited(23));
    }

    // WHY: pipe.c:139 - "Let the sending side handle this": a receiver that
    // also applied --chmod would apply it a second time.
    #[test]
    fn the_receiver_leaves_chmod_to_the_sender() {
        let config = ClientConfig::builder()
            .transfer_args(["src/", "dst/"])
            .chmod(Some(
                ::metadata::ChmodModifiers::parse("u+x").expect("valid --chmod"),
            ))
            .build();
        let receiver =
            local_receiver_config(&config, std::ffi::OsStr::new("dst/")).expect("receiver config");
        assert!(receiver.chmod.is_none());
        assert_eq!(receiver.role, ServerRole::Receiver);
        assert_eq!(receiver.args, ["dst/"]);
    }

    // WHY: the receiver's parallel work must stay inside the destination the
    // confinement allows, as it does in upstream's forked child. Work sent to
    // a pool whose threads existed before the confinement escapes it.
    #[cfg(target_os = "linux")]
    #[test]
    fn receiver_pool_threads_inherit_the_confinement() {
        if !fast_io::landlock::is_supported() {
            return;
        }
        let dest = tempfile::tempdir().expect("dest");
        let outside = tempfile::tempdir().expect("outside");
        // Workers that exist before the confinement, as the process-wide
        // pool's would.
        rayon::join(|| (), || ());
        let target = outside.path().join("escaped");
        let config = ClientConfig::builder()
            .transfer_args(["src/", dest.path().to_str().expect("utf-8")])
            .build();
        let receiver =
            local_receiver_config(&config, dest.path().as_os_str()).expect("receiver config");
        let denied = std::thread::spawn(move || {
            crate::session::confine_receiver(&receiver);
            run_confined(|| rayon::join(|| std::fs::write(&target, b"x"), || ()).0).expect("pool")
        })
        .join()
        .expect("receiver thread");
        let error = denied.expect_err("a receiver worker wrote outside the destination");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
