/// RAII guard that writes a PID file on creation and removes it on drop.
///
/// upstream: clientserver.c:1582 `create_pid_file()` writes the daemon PID after binding.
struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn create(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| pid_file_error(&path, error))?;
        }

        let mut file = open_pid_file(&path).map_err(|error| pid_file_error(&path, error))?;

        // upstream: clientserver.c:1628 creates the file 0664 under the
        // daemon's umask; oc pins 0644. Applied to the open descriptor, never
        // re-resolved through the path.
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|error| pid_file_error(&path, error))?;

        // upstream: clientserver.c:1654 truncates only after the opened file
        // has been verified, so a refused open never clobbers anything.
        file.set_len(0)
            .map_err(|error| pid_file_error(&path, error))?;
        let pid = std::process::id();
        writeln!(file, "{pid}").map_err(|error| pid_file_error(&path, error))?;
        file.sync_all()
            .map_err(|error| pid_file_error(&path, error))?;

        Ok(Self { path })
    }
}

/// Opens the PID file without ever writing through whatever sits at its leaf.
///
/// The PID file usually lives in a shared run directory, so its leaf can be a
/// symlink someone else planted there. upstream: clientserver.c:1636-1652
/// unlinks a leaf that is not a regular file, opens the leaf with
/// `O_NOFOLLOW`, and requires the opened file to be the regular file `lstat`
/// names (same device and inode) before anything is written. The parent
/// directory is operator-configured and followed (`/var/run -> /run`).
#[cfg(unix)]
fn open_pid_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    if let Ok(existing) = fs::symlink_metadata(path)
        && !existing.file_type().is_file()
    {
        fs::remove_file(path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !opened.file_type().is_file() || opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(io::Error::other(
            "the pid file was replaced while it was being opened",
        ));
    }
    Ok(file)
}

/// Non-Unix stub: there is no `O_NOFOLLOW` or owner model to apply.
#[cfg(not(unix))]
fn open_pid_file(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new().write(true).create(true).open(path)
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
