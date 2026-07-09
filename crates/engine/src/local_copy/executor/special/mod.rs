//! Copy logic for FIFOs, devices, and symbolic links.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord, overrides::create_hard_link, remove_existing_destination,
    remove_source_entry_if_requested,
};

mod device;
mod fifo;
mod symlink;

pub(crate) use device::copy_device;
pub(crate) use fifo::copy_fifo;
pub(crate) use symlink::{copy_symlink, create_symlink, symlink_target_is_safe};

/// Hard-links a `--link-dest` basis device or special node into place, recording
/// an exact-match `hD`/`hS` itemize row with blank attribute slots.
///
/// upstream: generator.c:1105-1137 try_dests_non() match_level 3 - the basis is
/// hard-linked via `do_link()` and itemized with
/// `ITEM_LOCAL_CHANGE|ITEM_XNAME_FOLLOWS|ITEM_MATCHED` and an empty xname, which
/// log.c:701-744 renders as `h` in position 0 with every attribute slot
/// collapsed to a space because nothing changed relative to the basis.
#[allow(clippy::too_many_arguments)]
pub(super) fn link_special_from_link_dest(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    basis: &Path,
    record_path: Option<&Path>,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    is_device: bool,
) -> Result<(), LocalCopyError> {
    let mut attempted_commit = false;
    loop {
        match create_hard_link(basis, destination) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(destination)?;
                create_hard_link(basis, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination.to_path_buf(), link_error)
                })?;
                break;
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && context.delay_updates_enabled()
                    && !attempted_commit =>
            {
                context.commit_deferred_update_for(basis)?;
                attempted_commit = true;
                continue;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create hard link",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }
    }

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_hard_link();
    if is_device {
        context.summary_mut().record_device();
    } else {
        context.summary_mut().record_fifo();
    }

    if let Some(path) = record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        // The hard-linked node shares the basis inode, so it matches the source
        // exactly: no `with_creation`, empty change-set, so log.c:735-744
        // collapses the attribute slots to spaces (`hD`/`hS` + blank).
        context.record(LocalCopyRecord::new(
            path.to_path_buf(),
            LocalCopyAction::HardLink,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }

    context.register_created_path(
        destination,
        CreatedEntryKind::HardLink,
        destination_previously_existed,
    );
    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path, file_type)?;
    Ok(())
}
