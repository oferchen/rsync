//! Async twins of the variable-length integer read leaves.
//!
//! Gated on the `tokio-transfer` feature. Each function is the `.await`-driven
//! counterpart of its blocking sibling in [`super::decode`]. The only
//! difference is how the tag byte and continuation bytes are pulled off the
//! wire (`.await` on [`AsyncRead`] versus a blocking `read_exact`); the decode
//! math (the `INT_BYTE_EXTRA` table lookup, the little-endian reconstruction,
//! the overflow guards) is byte-for-byte identical to the sync leaves, so the
//! two can never diverge on the value produced or the number of bytes consumed.
//!
//! Additive and unwired: these leaves are exercised only by the parity tests.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::table::{INT_BYTE_EXTRA, MAX_EXTRA_BYTES, invalid_data};

/// Async twin of [`read_varint`](super::read_varint).
///
/// Reads the leading tag byte, derives the continuation-byte count from
/// `INT_BYTE_EXTRA`, reads exactly that many extra bytes, and reconstructs the
/// value with the identical bit math the sync leaf uses.
#[inline]
pub async fn read_varint_async<R: AsyncRead + Unpin + ?Sized>(reader: &mut R) -> io::Result<i32> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first).await?;

    let extra = INT_BYTE_EXTRA[(first[0] / 4) as usize] as usize;
    if extra > MAX_EXTRA_BYTES {
        return Err(invalid_data("overflow in read_varint"));
    }

    let mut buf = [0u8; 5];
    if extra > 0 {
        reader.read_exact(&mut buf[..extra]).await?;
        let bit = 1u8 << (8 - extra as u32);
        buf[extra] = first[0] & (bit - 1);
    } else {
        buf[0] = first[0];
    }

    let value = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok(value)
}

/// Async twin of [`read_varlong`](super::read_varlong).
///
/// Reads `min_bytes` (leading tag plus `min_bytes - 1` initial data bytes),
/// derives the continuation count, reads the extra bytes, and reconstructs the
/// 64-bit value with the identical guards and masking the sync leaf uses.
#[inline]
pub async fn read_varlong_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    min_bytes: u8,
) -> io::Result<i64> {
    if min_bytes == 0 || min_bytes > 8 {
        return Err(invalid_data("invalid min_bytes in read_varlong"));
    }
    let min = min_bytes as usize;

    let mut initial = [0u8; 8];
    reader.read_exact(&mut initial[..min]).await?;

    let leading = initial[0];

    let mut result = [0u8; 9];
    result[..min - 1].copy_from_slice(&initial[1..min]);

    let extra = INT_BYTE_EXTRA[(leading / 4) as usize] as usize;

    if extra > 0 {
        if min + extra > 9 {
            return Err(invalid_data("overflow in read_varlong"));
        }
        let bit = 1u8 << (8 - extra as u32);
        reader
            .read_exact(&mut result[min - 1..min - 1 + extra])
            .await?;
        result[min + extra - 1] = leading & (bit - 1);
    } else {
        result[min - 1] = leading;
    }

    Ok(i64::from_le_bytes([
        result[0], result[1], result[2], result[3], result[4], result[5], result[6], result[7],
    ]))
}

/// Async twin of [`read_longint`](super::read_longint).
///
/// Reads the 4-byte prefix; if it is the `0xFFFFFFFF` marker, reads the full
/// 8-byte value, exactly as the sync leaf does.
pub async fn read_longint_async<R: AsyncRead + Unpin + ?Sized>(reader: &mut R) -> io::Result<i64> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    let first = i32::from_le_bytes(buf);

    if first == -1 {
        let mut buf64 = [0u8; 8];
        reader.read_exact(&mut buf64).await?;
        Ok(i64::from_le_bytes(buf64))
    } else {
        Ok(first as i64)
    }
}

/// Async twin of [`read_int`](super::read_int).
///
/// Reads the fixed 4-byte little-endian integer.
#[inline]
pub async fn read_int_async<R: AsyncRead + Unpin + ?Sized>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    Ok(i32::from_le_bytes(buf))
}
