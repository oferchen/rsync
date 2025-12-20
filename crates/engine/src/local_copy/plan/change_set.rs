use std::fs;
use std::time::SystemTime;

use ::metadata::MetadataOptions;

/// Describes the attributes that changed for a recorded local-copy action.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalCopyChangeSet {
    checksum_changed: bool,
    size_changed: bool,
    time_change: Option<TimeChange>,
    permissions_changed: bool,
    owner_changed: bool,
    group_changed: bool,
    access_time_changed: bool,
    create_time_changed: bool,
    acl_changed: bool,
    xattr_changed: bool,
}

/// Describes how the modification time was adjusted for an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeChange {
    /// The modification time now matches the sender's metadata.
    Modified,
    /// The modification time was set to the transfer time because preservation was disabled.
    TransferTime,
}

impl LocalCopyChangeSet {
    /// Returns a change-set with all flags cleared.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            checksum_changed: false,
            size_changed: false,
            time_change: None,
            permissions_changed: false,
            owner_changed: false,
            group_changed: false,
            access_time_changed: false,
            create_time_changed: false,
            acl_changed: false,
            xattr_changed: false,
        }
    }

    /// Marks whether the data checksum changed.
    #[must_use]
    pub const fn with_checksum_changed(mut self, changed: bool) -> Self {
        self.checksum_changed = changed;
        self
    }

    /// Marks whether the logical size changed.
    #[must_use]
    pub const fn with_size_changed(mut self, changed: bool) -> Self {
        self.size_changed = changed;
        self
    }

    /// Records how the modification time changed.
    #[must_use]
    pub const fn with_time_change(mut self, change: Option<TimeChange>) -> Self {
        self.time_change = change;
        self
    }

    /// Marks whether permissions changed.
    #[must_use]
    pub const fn with_permissions_changed(mut self, changed: bool) -> Self {
        self.permissions_changed = changed;
        self
    }

    /// Marks whether ownership changed.
    #[must_use]
    pub const fn with_owner_changed(mut self, changed: bool) -> Self {
        self.owner_changed = changed;
        self
    }

    /// Marks whether the group changed.
    #[must_use]
    pub const fn with_group_changed(mut self, changed: bool) -> Self {
        self.group_changed = changed;
        self
    }

    /// Marks whether the access time changed.
    #[must_use]
    pub const fn with_access_time_changed(mut self, changed: bool) -> Self {
        self.access_time_changed = changed;
        self
    }

    /// Marks whether the create time changed.
    #[must_use]
    pub const fn with_create_time_changed(mut self, changed: bool) -> Self {
        self.create_time_changed = changed;
        self
    }

    /// Marks whether ACLs changed.
    #[must_use]
    pub const fn with_acl_changed(mut self, changed: bool) -> Self {
        self.acl_changed = changed;
        self
    }

    /// Marks whether extended attributes changed.
    #[must_use]
    pub const fn with_xattr_changed(mut self, changed: bool) -> Self {
        self.xattr_changed = changed;
        self
    }

    /// Reports whether the file contents or equivalent metadata changed.
    #[must_use]
    pub const fn checksum_changed(&self) -> bool {
        self.checksum_changed
    }

    /// Reports whether the size changed.
    #[must_use]
    pub const fn size_changed(&self) -> bool {
        self.size_changed
    }

    /// Returns the recorded time change, if any.
    #[must_use]
    pub const fn time_change(&self) -> Option<TimeChange> {
        self.time_change
    }

    /// Returns the canonical itemize marker for the recorded time change, when any.
    #[must_use]
    pub const fn time_change_marker(&self) -> Option<char> {
        match self.time_change {
            Some(TimeChange::Modified) => Some('t'),
            Some(TimeChange::TransferTime) => Some('T'),
            None => None,
        }
    }

    /// Reports whether permissions changed.
    #[must_use]
    pub const fn permissions_changed(&self) -> bool {
        self.permissions_changed
    }

    /// Reports whether the owner changed.
    #[must_use]
    pub const fn owner_changed(&self) -> bool {
        self.owner_changed
    }

    /// Reports whether the group changed.
    #[must_use]
    pub const fn group_changed(&self) -> bool {
        self.group_changed
    }

    /// Reports whether the access time changed.
    #[must_use]
    pub const fn access_time_changed(&self) -> bool {
        self.access_time_changed
    }

    /// Reports whether the create time changed.
    #[must_use]
    pub const fn create_time_changed(&self) -> bool {
        self.create_time_changed
    }

    /// Reports whether ACL data changed.
    #[must_use]
    pub const fn acl_changed(&self) -> bool {
        self.acl_changed
    }

    /// Reports whether extended attributes changed.
    #[must_use]
    pub const fn xattr_changed(&self) -> bool {
        self.xattr_changed
    }

    /// Computes a change set for a file-like entry (regular files and symlinks).
    #[allow(clippy::too_many_arguments)]
    pub fn for_file(
        metadata: &fs::Metadata,
        existing: Option<&fs::Metadata>,
        metadata_options: &MetadataOptions,
        destination_previously_existed: bool,
        wrote_data: bool,
        xattrs_enabled: bool,
        acls_enabled: bool,
    ) -> Self {
        let mut change_set = Self::new();

        if wrote_data {
            change_set = change_set.with_checksum_changed(true);
        }

        if !destination_previously_existed {
            change_set = change_set.with_size_changed(true);
        } else if let Some(existing_metadata) = existing
            && metadata.len() != existing_metadata.len()
        {
            change_set = change_set.with_size_changed(true);
        }

        change_set = change_set.with_time_change(determine_time_change(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
            wrote_data,
        ));

        if metadata_options.permissions()
            && permissions_changed(metadata, existing, destination_previously_existed)
        {
            change_set = change_set.with_permissions_changed(true);
        }

        if metadata_options.chmod().is_some() {
            change_set = change_set.with_permissions_changed(true);
        }

        if owner_changed(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
        ) {
            change_set = change_set.with_owner_changed(true);
        }

        if group_changed(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
        ) {
            change_set = change_set.with_group_changed(true);
        }

        if metadata_options.user_mapping().is_some() {
            change_set = change_set.with_owner_changed(true);
        }

        if metadata_options.group_mapping().is_some() {
            change_set = change_set.with_group_changed(true);
        }

        if xattrs_enabled {
            change_set = change_set.with_xattr_changed(true);
        }

        if acls_enabled {
            change_set = change_set.with_acl_changed(true);
        }

        change_set
    }
}

fn determine_time_change(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    wrote_data: bool,
) -> Option<TimeChange> {
    if metadata_options.times() {
        if !destination_previously_existed {
            return Some(TimeChange::Modified);
        }

        let new_mtime = metadata_modified_time(metadata);
        let old_mtime = existing.and_then(metadata_modified_time);

        match (new_mtime, old_mtime) {
            (Some(new_value), Some(old_value)) if new_value == old_value => None,
            _ => Some(TimeChange::Modified),
        }
    } else if wrote_data || !destination_previously_existed {
        Some(TimeChange::TransferTime)
    } else {
        None
    }
}

fn permissions_changed(
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    let new_mode = metadata_mode(metadata);
    if !destination_previously_existed {
        return new_mode.is_some();
    }

    match (new_mode, existing.and_then(metadata_mode)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn owner_changed(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    if let Some(override_uid) = metadata_options.owner_override() {
        return existing.and_then(metadata_uid) != Some(override_uid);
    }

    if !metadata_options.owner() {
        return false;
    }

    let new_uid = metadata_uid(metadata);
    if !destination_previously_existed {
        return new_uid.is_some();
    }

    match (new_uid, existing.and_then(metadata_uid)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        _ => false,
    }
}

fn group_changed(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    if let Some(override_gid) = metadata_options.group_override() {
        return existing.and_then(metadata_gid) != Some(override_gid);
    }

    if !metadata_options.group() {
        return false;
    }

    let new_gid = metadata_gid(metadata);
    if !destination_previously_existed {
        return new_gid.is_some();
    }

    match (new_gid, existing.and_then(metadata_gid)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        _ => false,
    }
}

fn metadata_modified_time(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.modified().ok()
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode())
}

#[cfg(not(unix))]
fn metadata_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn metadata_uid(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.uid())
}

#[cfg(not(unix))]
fn metadata_uid(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn metadata_gid(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.gid())
}

#[cfg(not(unix))]
fn metadata_gid(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn change_set_detects_size_and_time_changes() {
        use filetime::{FileTime, set_file_mtime};

        let temp = tempfile::tempdir().expect("tempdir");
        let existing_path = temp.path().join("existing.txt");
        fs::write(&existing_path, b"old").expect("write existing");

        let epoch = FileTime::from_unix_time(1, 0);
        set_file_mtime(&existing_path, epoch).expect("set mtime");
        let existing = fs::metadata(&existing_path).expect("metadata");

        let new_path = temp.path().join("new.txt");
        fs::write(&new_path, b"new data").expect("write new");
        let metadata = fs::metadata(&new_path).expect("metadata");

        let options = MetadataOptions::new();
        let change_set = LocalCopyChangeSet::for_file(
            &metadata,
            Some(&existing),
            &options,
            true,
            true,
            false,
            false,
        );

        assert!(change_set.checksum_changed());
        assert!(change_set.size_changed());
        assert!(matches!(
            change_set.time_change(),
            Some(TimeChange::Modified)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn change_set_detects_permission_changes_for_existing_destination() {
        use filetime::{FileTime, set_file_mtime};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let existing_path = temp.path().join("existing.txt");
        fs::write(&existing_path, b"content").expect("write existing");
        let mut existing_perms = fs::metadata(&existing_path)
            .expect("metadata")
            .permissions();
        existing_perms.set_mode(0o644);
        fs::set_permissions(&existing_path, existing_perms).expect("set existing perms");
        let existing = fs::metadata(&existing_path).expect("metadata");
        let existing_mtime =
            FileTime::from_system_time(existing.modified().expect("existing mtime"));

        let new_path = temp.path().join("updated.txt");
        fs::write(&new_path, b"content").expect("write new");
        let mut new_perms = fs::metadata(&new_path).expect("metadata").permissions();
        new_perms.set_mode(0o600);
        fs::set_permissions(&new_path, new_perms).expect("set new perms");
        set_file_mtime(&new_path, existing_mtime).expect("align mtime");
        let metadata = fs::metadata(&new_path).expect("metadata");

        assert_ne!(metadata.mode(), existing.mode());

        let options = MetadataOptions::new();
        let change_set = LocalCopyChangeSet::for_file(
            &metadata,
            Some(&existing),
            &options,
            true,
            false,
            false,
            false,
        );

        assert!(change_set.permissions_changed());
        assert!(!change_set.size_changed());
        assert!(change_set.time_change().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn change_set_detects_owner_override_mismatch() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        fs::write(&path, b"content").expect("write file");
        let metadata = fs::metadata(&path).expect("metadata");

        let current_uid = metadata.uid();
        let override_uid = if current_uid == u32::MAX {
            current_uid - 1
        } else {
            current_uid + 1
        };

        let options = MetadataOptions::new()
            .preserve_owner(true)
            .with_owner_override(Some(override_uid));
        let change_set = LocalCopyChangeSet::for_file(
            &metadata,
            Some(&metadata),
            &options,
            true,
            false,
            false,
            false,
        );

        assert!(change_set.owner_changed());
    }

    #[cfg(unix)]
    #[test]
    fn change_set_detects_group_override_mismatch() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        fs::write(&path, b"content").expect("write file");
        let metadata = fs::metadata(&path).expect("metadata");

        let current_gid = metadata.gid();
        let override_gid = if current_gid == u32::MAX {
            current_gid - 1
        } else {
            current_gid + 1
        };

        let options = MetadataOptions::new()
            .preserve_group(true)
            .with_group_override(Some(override_gid));
        let change_set = LocalCopyChangeSet::for_file(
            &metadata,
            Some(&metadata),
            &options,
            true,
            false,
            false,
            false,
        );

        assert!(change_set.group_changed());
    }
}
