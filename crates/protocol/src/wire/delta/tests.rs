#![deny(unsafe_code)]

use std::io::{self, Read};

use super::*;

#[test]
fn delta_op_roundtrip_literal() {
    let op = DeltaOp::Literal(vec![0x01, 0x02, 0x03, 0x04, 0x05]);

    let mut buf = Vec::new();
    write_delta_op(&mut buf, &op).unwrap();

    let decoded = read_delta_op(&mut &buf[..]).unwrap();

    assert_eq!(decoded, op);
}

#[test]
fn delta_op_roundtrip_copy() {
    let op = DeltaOp::Copy {
        block_index: 42,
        length: 4096,
    };

    let mut buf = Vec::new();
    write_delta_op(&mut buf, &op).unwrap();

    let decoded = read_delta_op(&mut &buf[..]).unwrap();

    assert_eq!(decoded, op);
}

#[test]
fn delta_stream_roundtrip_mixed_ops() {
    let ops = vec![
        DeltaOp::Literal(vec![0x01, 0x02, 0x03]),
        DeltaOp::Copy {
            block_index: 0,
            length: 1024,
        },
        DeltaOp::Literal(vec![0x04, 0x05]),
        DeltaOp::Copy {
            block_index: 5,
            length: 2048,
        },
        DeltaOp::Literal(vec![0x06]),
    ];

    let mut buf = Vec::new();
    write_delta(&mut buf, &ops).unwrap();

    let decoded = read_delta(&mut &buf[..]).unwrap();

    assert_eq!(decoded.len(), ops.len());
    for (i, (decoded_op, expected_op)) in decoded.iter().zip(ops.iter()).enumerate() {
        assert_eq!(decoded_op, expected_op, "mismatch at op {i}");
    }
}

#[test]
fn delta_stream_empty() {
    let ops: Vec<DeltaOp> = vec![];

    let mut buf = Vec::new();
    write_delta(&mut buf, &ops).unwrap();

    let decoded = read_delta(&mut &buf[..]).unwrap();

    assert_eq!(decoded.len(), 0);
}

#[test]
fn delta_op_rejects_invalid_opcode() {
    let buf = [0xFF, 0x00, 0x00, 0x00];
    let result = read_delta_op(&mut &buf[..]);

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("invalid delta opcode")
    );
}

#[test]
fn delta_stream_single_large_literal() {
    let data = vec![0x42; 65536];
    let ops = vec![DeltaOp::Literal(data.clone())];

    let mut buf = Vec::new();
    write_delta(&mut buf, &ops).unwrap();

    let decoded = read_delta(&mut &buf[..]).unwrap();

    assert_eq!(decoded.len(), 1);
    if let DeltaOp::Literal(decoded_data) = &decoded[0] {
        assert_eq!(decoded_data.len(), 65536);
        assert_eq!(decoded_data, &data);
    } else {
        panic!("expected Literal operation");
    }
}

#[test]
fn write_int_roundtrip() {
    let values = [0i32, 1, -1, 127, -128, 1000, -1000, i32::MAX, i32::MIN];
    for &value in &values {
        let mut buf = Vec::new();
        write_int(&mut buf, value).unwrap();
        assert_eq!(buf.len(), 4);
        let decoded = read_int(&mut &buf[..]).unwrap();
        assert_eq!(decoded, value, "roundtrip failed for {value}");
    }
}

#[test]
fn write_int_little_endian() {
    let mut buf = Vec::new();
    write_int(&mut buf, 0x12345678).unwrap();
    assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);
}

#[test]
fn write_token_literal_small() {
    let data = b"hello";
    let mut buf = Vec::new();
    write_token_literal(&mut buf, data).unwrap();

    assert_eq!(buf.len(), 4 + 5);
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, 5);
    assert_eq!(&buf[4..], b"hello");
}

#[test]
fn write_token_literal_chunked() {
    let data = vec![0x42u8; CHUNK_SIZE + 100];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + 100);

    let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len1, CHUNK_SIZE as i32);

    let second_header_start = 4 + CHUNK_SIZE;
    let len2 = i32::from_le_bytes([
        buf[second_header_start],
        buf[second_header_start + 1],
        buf[second_header_start + 2],
        buf[second_header_start + 3],
    ]);
    assert_eq!(len2, 100);
}

#[test]
fn write_token_block_match_encoding() {
    let mut buf = Vec::new();
    write_token_block_match(&mut buf, 0).unwrap();
    assert_eq!(buf, (-1i32).to_le_bytes());

    buf.clear();
    write_token_block_match(&mut buf, 1).unwrap();
    assert_eq!(buf, (-2i32).to_le_bytes());

    buf.clear();
    write_token_block_match(&mut buf, 42).unwrap();
    assert_eq!(buf, (-43i32).to_le_bytes());
}

#[test]
fn write_token_end_is_zero() {
    let mut buf = Vec::new();
    write_token_end(&mut buf).unwrap();
    assert_eq!(buf, [0, 0, 0, 0]);
}

#[test]
fn write_whole_file_delta_format() {
    let data = b"test data";
    let mut buf = Vec::new();
    write_whole_file_delta(&mut buf, data).unwrap();

    assert_eq!(buf.len(), 4 + 9 + 4);

    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, 9);

    assert_eq!(&buf[4..13], b"test data");

    let end = i32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]);
    assert_eq!(end, 0);
}

#[test]
fn read_token_parses_literals_and_blocks() {
    let mut buf = 17i32.to_le_bytes().to_vec();
    let token = read_token(&mut &buf[..]).unwrap();
    assert_eq!(token, Some(17));

    buf = (-1i32).to_le_bytes().to_vec();
    let token = read_token(&mut &buf[..]).unwrap();
    assert_eq!(token, Some(-1));

    buf = 0i32.to_le_bytes().to_vec();
    let token = read_token(&mut &buf[..]).unwrap();
    assert_eq!(token, None);
}

#[test]
fn write_token_stream_mixed_ops() {
    let ops = vec![
        DeltaOp::Literal(b"hello".to_vec()),
        DeltaOp::Copy {
            block_index: 0,
            length: 1024,
        },
        DeltaOp::Literal(b"world".to_vec()),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let mut cursor = &buf[..];

    let len1 = read_int(&mut cursor).unwrap();
    assert_eq!(len1, 5);
    let mut data1 = [0u8; 5];
    cursor.read_exact(&mut data1).unwrap();
    assert_eq!(&data1, b"hello");

    let block = read_int(&mut cursor).unwrap();
    assert_eq!(block, -1);

    let len2 = read_int(&mut cursor).unwrap();
    assert_eq!(len2, 5);
    let mut data2 = [0u8; 5];
    cursor.read_exact(&mut data2).unwrap();
    assert_eq!(&data2, b"world");

    let end = read_int(&mut cursor).unwrap();
    assert_eq!(end, 0);

    assert!(cursor.is_empty());
}

/// Decodes a token stream and reconstructs literal data.
///
/// Returns (literals, block_indices) where literals is concatenated literal data
/// and block_indices contains the block references encountered.
fn decode_token_stream(data: &[u8]) -> io::Result<(Vec<u8>, Vec<u32>)> {
    let mut cursor = data;
    let mut literals = Vec::new();
    let mut block_indices = Vec::new();

    loop {
        match read_token(&mut cursor)? {
            None => break,
            Some(token) if token > 0 => {
                let len = token as usize;
                let mut chunk = vec![0u8; len];
                cursor.read_exact(&mut chunk)?;
                literals.extend_from_slice(&chunk);
            }
            Some(token) => {
                // Block match: token is -(block_index + 1)
                let block_index = (-(token + 1)) as u32;
                block_indices.push(block_index);
            }
        }
    }

    Ok((literals, block_indices))
}

#[test]
fn delta_oversized_literal_exactly_chunk_size() {
    let data = vec![0xABu8; CHUNK_SIZE];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), 4 + CHUNK_SIZE);

    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, CHUNK_SIZE as i32);

    assert!(buf[4..].iter().all(|&b| b == 0xAB));
}

#[test]
fn delta_oversized_literal_one_byte_over_chunk_size() {
    let data = vec![0xCDu8; CHUNK_SIZE + 1];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + 1);

    let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len1, CHUNK_SIZE as i32);

    let second_header_start = 4 + CHUNK_SIZE;
    let len2 = i32::from_le_bytes([
        buf[second_header_start],
        buf[second_header_start + 1],
        buf[second_header_start + 2],
        buf[second_header_start + 3],
    ]);
    assert_eq!(len2, 1);

    assert_eq!(buf[second_header_start + 4], 0xCD);
}

#[test]
fn delta_oversized_literal_multiple_chunks() {
    let data = vec![0xEFu8; CHUNK_SIZE * 3];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 3);

    for i in 0..3 {
        let offset = i * (4 + CHUNK_SIZE);
        let len = i32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        assert_eq!(len, CHUNK_SIZE as i32, "chunk {i} header mismatch");
    }
}

#[test]
fn delta_oversized_literal_multiple_chunks_with_remainder() {
    let remainder = CHUNK_SIZE / 2;
    let data = vec![0x12u8; CHUNK_SIZE * 2 + remainder];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2 + 4 + remainder);

    for i in 0..2 {
        let offset = i * (4 + CHUNK_SIZE);
        let len = i32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        assert_eq!(len, CHUNK_SIZE as i32);
    }

    let third_offset = 2 * (4 + CHUNK_SIZE);
    let len3 = i32::from_le_bytes([
        buf[third_offset],
        buf[third_offset + 1],
        buf[third_offset + 2],
        buf[third_offset + 3],
    ]);
    assert_eq!(len3, remainder as i32);
}

#[test]
fn delta_oversized_literal_reconstruction() {
    let size = CHUNK_SIZE * 2 + 1234;
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        data.push((i % 256) as u8);
    }

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

    assert!(block_indices.is_empty(), "should have no block references");
    assert_eq!(reconstructed.len(), data.len());
    assert_eq!(
        reconstructed, data,
        "reconstructed data should match original"
    );
}

#[test]
fn delta_oversized_literal_reconstruction_exact_multiple() {
    let size = CHUNK_SIZE * 4;
    let data: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(7)).collect();

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();

    assert_eq!(reconstructed.len(), data.len());
    assert_eq!(reconstructed, data);
}

#[test]
fn delta_oversized_literal_mixed_with_blocks() {
    let large_literal = vec![0xAAu8; CHUNK_SIZE + 500];
    let small_literal = b"small".to_vec();

    let ops = vec![
        DeltaOp::Literal(large_literal.clone()),
        DeltaOp::Copy {
            block_index: 0,
            length: 4096,
        },
        DeltaOp::Literal(small_literal.clone()),
        DeltaOp::Copy {
            block_index: 5,
            length: 4096,
        },
        DeltaOp::Literal(vec![0xBBu8; CHUNK_SIZE * 2]),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

    assert_eq!(block_indices, vec![0, 5]);

    let expected_literal_size = large_literal.len() + small_literal.len() + CHUNK_SIZE * 2;
    assert_eq!(reconstructed_literals.len(), expected_literal_size);

    assert_eq!(
        &reconstructed_literals[..large_literal.len()],
        &large_literal[..]
    );

    let small_start = large_literal.len();
    assert_eq!(
        &reconstructed_literals[small_start..small_start + small_literal.len()],
        &small_literal[..]
    );

    let last_start = small_start + small_literal.len();
    assert!(
        reconstructed_literals[last_start..]
            .iter()
            .all(|&b| b == 0xBB)
    );
}

#[test]
fn delta_oversized_literal_via_whole_file() {
    let size = CHUNK_SIZE * 3 + 789;
    let data: Vec<u8> = (0..size).map(|i| ((i * 13) % 256) as u8).collect();

    let mut buf = Vec::new();
    write_whole_file_delta(&mut buf, &data).unwrap();

    let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

    assert!(block_indices.is_empty());
    assert_eq!(reconstructed, data);
}

#[test]
fn delta_oversized_literal_empty() {
    let data: Vec<u8> = vec![];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert!(buf.is_empty());
}

#[test]
fn delta_oversized_literal_single_byte() {
    let data = vec![0x42u8];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), 4 + 1);
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, 1);
    assert_eq!(buf[4], 0x42);
}

#[test]
fn delta_oversized_literal_chunk_boundary_minus_one() {
    let data = vec![0x99u8; CHUNK_SIZE - 1];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), 4 + CHUNK_SIZE - 1);

    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, (CHUNK_SIZE - 1) as i32);
}

#[test]
fn delta_oversized_literal_very_large() {
    let size = 1024 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();
    assert_eq!(reconstructed.len(), size);
    assert_eq!(reconstructed, data);

    let expected_chunks = size.div_ceil(CHUNK_SIZE);
    assert_eq!(expected_chunks, 32);
}

#[test]
fn delta_oversized_literal_data_integrity() {
    let size = CHUNK_SIZE * 2 + CHUNK_SIZE / 2;
    let mut data = Vec::with_capacity(size);

    for i in 0..CHUNK_SIZE {
        data.push((i % 256) as u8);
    }
    for i in 0..CHUNK_SIZE {
        data.push((255 - (i % 256)) as u8);
    }
    for i in 0..(CHUNK_SIZE / 2) {
        data.push(if i % 2 == 0 { 0xAA } else { 0x55 });
    }

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();

    for (i, &byte) in reconstructed.iter().enumerate().take(CHUNK_SIZE) {
        assert_eq!(byte, (i % 256) as u8, "first chunk byte {i} mismatch");
    }
    for (i, &byte) in reconstructed
        .iter()
        .skip(CHUNK_SIZE)
        .enumerate()
        .take(CHUNK_SIZE)
    {
        assert_eq!(
            byte,
            (255 - (i % 256)) as u8,
            "second chunk byte {i} mismatch"
        );
    }
    for i in 0..(CHUNK_SIZE / 2) {
        let expected = if i % 2 == 0 { 0xAA } else { 0x55 };
        assert_eq!(
            reconstructed[CHUNK_SIZE * 2 + i],
            expected,
            "third chunk byte {i} mismatch"
        );
    }
}

#[test]
fn delta_stream_with_consecutive_oversized_literals() {
    let literal1 = vec![0x11u8; CHUNK_SIZE + 100];
    let literal2 = vec![0x22u8; CHUNK_SIZE * 2 + 200];
    let literal3 = vec![0x33u8; CHUNK_SIZE + 50];

    let ops = vec![
        DeltaOp::Literal(literal1.clone()),
        DeltaOp::Literal(literal2.clone()),
        DeltaOp::Literal(literal3.clone()),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

    assert!(block_indices.is_empty());

    let total_size = literal1.len() + literal2.len() + literal3.len();
    assert_eq!(reconstructed.len(), total_size);

    let mut offset = 0;
    assert!(
        reconstructed[offset..offset + literal1.len()]
            .iter()
            .all(|&b| b == 0x11)
    );
    offset += literal1.len();

    assert!(
        reconstructed[offset..offset + literal2.len()]
            .iter()
            .all(|&b| b == 0x22)
    );
    offset += literal2.len();

    assert!(
        reconstructed[offset..offset + literal3.len()]
            .iter()
            .all(|&b| b == 0x33)
    );
}

#[test]
fn chunk_boundary_exact_double_chunk_size() {
    let data = vec![0xDDu8; CHUNK_SIZE * 2];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2);

    let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len1, CHUNK_SIZE as i32);

    let offset2 = 4 + CHUNK_SIZE;
    let len2 = i32::from_le_bytes([
        buf[offset2],
        buf[offset2 + 1],
        buf[offset2 + 2],
        buf[offset2 + 3],
    ]);
    assert_eq!(len2, CHUNK_SIZE as i32);

    assert!(buf[4..4 + CHUNK_SIZE].iter().all(|&b| b == 0xDD));
    assert!(buf[offset2 + 4..].iter().all(|&b| b == 0xDD));
}

#[test]
fn chunk_boundary_two_chunks_plus_one_byte() {
    let data = vec![0xEEu8; CHUNK_SIZE * 2 + 1];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2 + 4 + 1);

    let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len1, CHUNK_SIZE as i32);

    let offset2 = 4 + CHUNK_SIZE;
    let len2 = i32::from_le_bytes([
        buf[offset2],
        buf[offset2 + 1],
        buf[offset2 + 2],
        buf[offset2 + 3],
    ]);
    assert_eq!(len2, CHUNK_SIZE as i32);

    let offset3 = (4 + CHUNK_SIZE) * 2;
    let len3 = i32::from_le_bytes([
        buf[offset3],
        buf[offset3 + 1],
        buf[offset3 + 2],
        buf[offset3 + 3],
    ]);
    assert_eq!(len3, 1);

    assert_eq!(buf[offset3 + 4], 0xEE);
}

#[test]
fn chunk_boundary_split_verification() {
    let mut data = Vec::new();

    for i in 0..CHUNK_SIZE {
        data.push((i & 0xFF) as u8);
    }

    for i in 0..CHUNK_SIZE {
        data.push(((i + 128) & 0xFF) as u8);
    }

    data.extend(std::iter::repeat_n(0xCC, 1000));

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();
    assert_eq!(reconstructed, data);

    for (i, &byte) in reconstructed.iter().enumerate().take(CHUNK_SIZE) {
        assert_eq!(byte, (i & 0xFF) as u8, "first chunk mismatch at {i}");
    }
    for (i, &byte) in reconstructed
        .iter()
        .skip(CHUNK_SIZE)
        .enumerate()
        .take(CHUNK_SIZE)
    {
        assert_eq!(
            byte,
            ((i + 128) & 0xFF) as u8,
            "second chunk mismatch at {i}"
        );
    }
    for (i, &byte) in reconstructed
        .iter()
        .skip(CHUNK_SIZE * 2)
        .enumerate()
        .take(1000)
    {
        assert_eq!(byte, 0xCC, "third chunk mismatch at {i}");
    }
}

#[test]
fn chunk_boundary_reassembly_interleaved_with_blocks() {
    let literal1 = vec![0xF1u8; CHUNK_SIZE + 10];
    let literal2 = vec![0xF2u8; CHUNK_SIZE * 3 + 20];

    let ops = vec![
        DeltaOp::Literal(vec![0xAAu8; 100]),
        DeltaOp::Copy {
            block_index: 0,
            length: 4096,
        },
        DeltaOp::Literal(literal1.clone()),
        DeltaOp::Copy {
            block_index: 1,
            length: 4096,
        },
        DeltaOp::Copy {
            block_index: 2,
            length: 4096,
        },
        DeltaOp::Literal(literal2.clone()),
        DeltaOp::Copy {
            block_index: 3,
            length: 4096,
        },
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

    assert_eq!(block_indices, vec![0, 1, 2, 3]);

    let expected_size = 100 + literal1.len() + literal2.len();
    assert_eq!(reconstructed_literals.len(), expected_size);

    assert!(reconstructed_literals[..100].iter().all(|&b| b == 0xAA));
    assert!(
        reconstructed_literals[100..100 + literal1.len()]
            .iter()
            .all(|&b| b == 0xF1)
    );
    assert!(
        reconstructed_literals[100 + literal1.len()..]
            .iter()
            .all(|&b| b == 0xF2)
    );
}

#[test]
fn chunk_boundary_max_i32_size_handling() {
    assert!(CHUNK_SIZE < i32::MAX as usize);

    let data = vec![0x77u8; CHUNK_SIZE];
    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();

    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert!(len > 0, "chunk size should be positive");
    assert_eq!(len, CHUNK_SIZE as i32);
}

#[test]
fn chunk_boundary_many_small_chunks_edge() {
    let size = CHUNK_SIZE * 10;
    let mut data = Vec::with_capacity(size);
    for i in 0..size {
        data.push((i / CHUNK_SIZE) as u8);
    }

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();
    assert_eq!(reconstructed.len(), size);
    assert_eq!(reconstructed, data);

    for chunk_idx in 0..10 {
        let start = chunk_idx * CHUNK_SIZE;
        let end = start + CHUNK_SIZE;
        let expected_value = chunk_idx as u8;
        assert!(
            reconstructed[start..end]
                .iter()
                .all(|&b| b == expected_value),
            "chunk {chunk_idx} has wrong value"
        );
    }
}

#[test]
fn chunk_boundary_off_by_one_before_boundary() {
    for offset in [2, 1, 0] {
        let size = CHUNK_SIZE - offset;
        let data = vec![0x88u8; size];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        assert_eq!(buf.len(), 4 + size, "size {size} failed");

        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, size as i32);
    }
}

#[test]
fn chunk_boundary_off_by_one_after_boundary() {
    for offset in [0, 1, 2] {
        let size = CHUNK_SIZE + offset;
        let data = vec![0x99u8; size];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        if offset == 0 {
            assert_eq!(buf.len(), 4 + CHUNK_SIZE);
            let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            assert_eq!(len, CHUNK_SIZE as i32);
        } else {
            assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + offset);

            let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            assert_eq!(len1, CHUNK_SIZE as i32);

            let offset2 = 4 + CHUNK_SIZE;
            let len2 = i32::from_le_bytes([
                buf[offset2],
                buf[offset2 + 1],
                buf[offset2 + 2],
                buf[offset2 + 3],
            ]);
            assert_eq!(len2, offset as i32);
        }
    }
}

#[test]
fn chunk_boundary_streaming_reconstruction() {
    let data: Vec<u8> = (0..CHUNK_SIZE * 2 + 500).map(|i| (i % 251) as u8).collect();

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let mut cursor = &buf[..];
    let mut reconstructed = Vec::new();

    loop {
        match read_token(&mut cursor).unwrap() {
            None => break,
            Some(token) if token > 0 => {
                let len = token as usize;
                let mut chunk = vec![0u8; len];
                cursor.read_exact(&mut chunk).unwrap();
                reconstructed.extend_from_slice(&chunk);
            }
            Some(_) => panic!("unexpected block token"),
        }
    }

    assert_eq!(reconstructed, data);
}

#[test]
fn chunk_boundary_alternating_pattern_integrity() {
    let size = CHUNK_SIZE * 2 + 100;
    let data: Vec<u8> = (0..size)
        .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
        .collect();

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();

    assert_eq!(reconstructed.len(), data.len());
    for (i, (&reconstructed_byte, &original_byte)) in
        reconstructed.iter().zip(data.iter()).enumerate()
    {
        assert_eq!(reconstructed_byte, original_byte, "mismatch at byte {i}");
    }
}

#[test]
fn chunk_boundary_zero_filled_chunks() {
    let data = vec![0x00u8; CHUNK_SIZE * 3];

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();
    assert_eq!(reconstructed.len(), data.len());
    assert!(reconstructed.iter().all(|&b| b == 0x00));
}

#[test]
fn chunk_boundary_all_different_bytes() {
    let mut data = Vec::new();
    let repetitions = (CHUNK_SIZE * 2) / 256 + 1;
    for _ in 0..repetitions {
        for b in 0..=255u8 {
            data.push(b);
        }
    }
    data.truncate(CHUNK_SIZE * 2 + 50);

    let mut buf = Vec::new();
    write_token_literal(&mut buf, &data).unwrap();
    write_token_end(&mut buf).unwrap();

    let (reconstructed, _) = decode_token_stream(&buf).unwrap();
    assert_eq!(reconstructed, data);
}

#[test]
fn chunk_boundary_stress_test_many_operations() {
    let ops = vec![
        DeltaOp::Literal(vec![0x01u8; CHUNK_SIZE + 1]),
        DeltaOp::Copy {
            block_index: 0,
            length: 4096,
        },
        DeltaOp::Literal(vec![0x02u8; CHUNK_SIZE * 2]),
        DeltaOp::Copy {
            block_index: 1,
            length: 4096,
        },
        DeltaOp::Literal(vec![0x03u8; CHUNK_SIZE - 1]),
        DeltaOp::Copy {
            block_index: 2,
            length: 4096,
        },
        DeltaOp::Literal(vec![0x04u8; CHUNK_SIZE]),
        DeltaOp::Copy {
            block_index: 3,
            length: 4096,
        },
        DeltaOp::Literal(vec![0x05u8; CHUNK_SIZE + 100]),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

    assert_eq!(block_indices, vec![0, 1, 2, 3]);

    let expected_size =
        (CHUNK_SIZE + 1) + (CHUNK_SIZE * 2) + (CHUNK_SIZE - 1) + CHUNK_SIZE + (CHUNK_SIZE + 100);
    assert_eq!(reconstructed_literals.len(), expected_size);

    let mut offset = 0;
    let sizes = [
        (CHUNK_SIZE + 1, 0x01u8),
        (CHUNK_SIZE * 2, 0x02u8),
        (CHUNK_SIZE - 1, 0x03u8),
        (CHUNK_SIZE, 0x04u8),
        (CHUNK_SIZE + 100, 0x05u8),
    ];

    for (size, expected_byte) in sizes.iter() {
        assert!(
            reconstructed_literals[offset..offset + size]
                .iter()
                .all(|&b| b == *expected_byte),
            "literal at offset {offset} with size {size} has wrong value"
        );
        offset += size;
    }
}

#[test]
fn chunk_boundary_write_read_symmetry() {
    let test_sizes = vec![
        0,
        1,
        CHUNK_SIZE - 1,
        CHUNK_SIZE,
        CHUNK_SIZE + 1,
        CHUNK_SIZE * 2,
        CHUNK_SIZE * 2 + 1,
        CHUNK_SIZE * 5 + 12345,
    ];

    for size in test_sizes {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();

        assert_eq!(
            reconstructed.len(),
            data.len(),
            "size mismatch for input size {size}"
        );
        assert_eq!(reconstructed, data, "data mismatch for input size {size}");
    }
}
