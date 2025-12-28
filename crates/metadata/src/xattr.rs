use crate::error::MetadataError;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::Path;

fn map_xattr_error(context: &'static str, path: &Path, error: io::Error) -> MetadataError {
    MetadataError::new(context, path, error)
}

fn list_attributes(path: &Path, follow_symlinks: bool) -> Result<Vec<OsString>, MetadataError> {
    let attrs = if follow_symlinks {
        xattr::list_deref(path)
    } else {
        xattr::list(path)
    }
    .map_err(|error| map_xattr_error("list extended attributes", path, error))?;
    Ok(attrs.collect())
}

fn read_attribute(
    path: &Path,
    name: &OsString,
    follow_symlinks: bool,
) -> Result<Option<Vec<u8>>, MetadataError> {
    let result = if follow_symlinks {
        xattr::get_deref(path, name)
    } else {
        xattr::get(path, name)
    };
    result.map_err(|error| map_xattr_error("read extended attribute", path, error))
}

fn write_attribute(
    path: &Path,
    name: &OsString,
    value: &[u8],
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let result = if follow_symlinks {
        xattr::set_deref(path, name, value)
    } else {
        xattr::set(path, name, value)
    };
    result.map_err(|error| map_xattr_error("write extended attribute", path, error))
}

fn remove_attribute(
    path: &Path,
    name: &OsString,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let result = if follow_symlinks {
        xattr::remove_deref(path, name)
    } else {
        xattr::remove(path, name)
    };
    result.map_err(|error| map_xattr_error("remove extended attribute", path, error))
}

/// Synchronises the extended attributes from `source` to `destination`.
pub fn sync_xattrs(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<(), MetadataError> {
    let source_attrs = list_attributes(source, follow_symlinks)?;
    let mut retained = HashSet::with_capacity(source_attrs.len());

    for name in &source_attrs {
        retained.insert(name.clone());
        let allow = filter
            .is_none_or(|predicate| predicate(&name.to_string_lossy()));

        if !allow {
            continue;
        }

        if let Some(value) = read_attribute(source, name, follow_symlinks)? {
            write_attribute(destination, name, &value, follow_symlinks)?;
        } else {
            remove_attribute(destination, name, follow_symlinks)?;
        }
    }

    let destination_attrs = list_attributes(destination, follow_symlinks)?;
    for name in &destination_attrs {
        if retained.contains(name) {
            continue;
        }

        let allow = filter
            .is_none_or(|predicate| predicate(&name.to_string_lossy()));

        if allow {
            remove_attribute(destination, name, follow_symlinks)?;
        }
    }

    Ok(())
}
