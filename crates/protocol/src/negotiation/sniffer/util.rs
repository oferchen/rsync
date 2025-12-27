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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_into_vectored_copies_bytes() {
        let source = b"hello world";
        let mut buf1 = [0u8; 6];
        let mut buf2 = [0u8; 6];
        let mut slices = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];
        let result = copy_into_vectored(source, &mut slices);
        assert!(result.is_ok());
        assert_eq!(&buf1, b"hello ");
        assert_eq!(&buf2[..5], b"world");
    }

    #[test]
    fn copy_into_vectored_single_buffer() {
        let source = b"test";
        let mut buf = [0u8; 10];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let result = copy_into_vectored(source, &mut slices);
        assert!(result.is_ok());
        assert_eq!(&buf[..4], b"test");
    }

    #[test]
    fn copy_into_vectored_exact_fit() {
        let source = b"abc";
        let mut buf = [0u8; 3];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let result = copy_into_vectored(source, &mut slices);
        assert!(result.is_ok());
        assert_eq!(&buf, b"abc");
    }

    #[test]
    fn copy_into_vectored_empty_source() {
        let source = b"";
        let mut buf = [0u8; 5];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let result = copy_into_vectored(source, &mut slices);
        assert!(result.is_ok());
    }

    #[test]
    fn copy_into_vectored_insufficient_space() {
        let source = b"hello world";
        let mut buf = [0u8; 5];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let result = copy_into_vectored(source, &mut slices);
        assert!(result.is_err());
    }

    #[test]
    fn ensure_vec_capacity_already_sufficient() {
        let mut vec = Vec::with_capacity(100);
        vec.push(1);
        let result = ensure_vec_capacity(&mut vec, 50);
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_vec_capacity_grows_vec() {
        let mut vec: Vec<u8> = Vec::new();
        let result = ensure_vec_capacity(&mut vec, 100);
        assert!(result.is_ok());
        assert!(vec.capacity() >= 100);
    }

    #[test]
    fn ensure_vec_capacity_zero_required() {
        let mut vec: Vec<u8> = Vec::new();
        let result = ensure_vec_capacity(&mut vec, 0);
        assert!(result.is_ok());
    }
}
