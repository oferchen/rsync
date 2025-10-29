use std::collections::TryReserveError;
use std::io::IoSliceMut;

use super::super::BufferedPrefixTooSmall;

pub(super) fn copy_into_vectored<'a>(
    source: &[u8],
    targets: &mut [IoSliceMut<'a>],
) -> Result<(), BufferedPrefixTooSmall> {
    let required = source.len();
    let available: usize = targets.iter().map(|buf| buf.len()).sum();
    if available < required {
        return Err(BufferedPrefixTooSmall::new(required, available));
    }

    let mut offset = 0;
    for buf in targets.iter_mut() {
        if offset == required {
            break;
        }

        let slice: &mut [u8] = buf.as_mut();
        let to_copy = slice.len().min(required - offset);
        slice[..to_copy].copy_from_slice(&source[offset..offset + to_copy]);
        offset += to_copy;
    }

    debug_assert_eq!(offset, required);
    Ok(())
}

pub(super) fn ensure_vec_capacity(
    target: &mut Vec<u8>,
    required: usize,
) -> Result<(), TryReserveError> {
    if target.capacity() < required {
        debug_assert!(
            target.len() < required,
            "destination length must be smaller than the required capacity when reserving",
        );
        let additional = required.saturating_sub(target.len());
        if additional > 0 {
            target.try_reserve_exact(additional)?;
        }
    }

    Ok(())
}
