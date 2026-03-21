use super::*;
use crate::strong::{Md5, Sha256, Xxh3};
use std::io::Cursor;

#[test]
fn test_sequential_checksum_empty_input() {
    let inputs: Vec<ChecksumInput<Cursor<Vec<u8>>>> = vec![];
    let config = PipelineConfig::default();

    let results = sequential_checksum::<Md5, _>(inputs, config).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_sequential_checksum_single_file() {
    let data = vec![0x42; 1024];
    let inputs = vec![ChecksumInput::new(Cursor::new(data.clone()), 1024)];
    let config = PipelineConfig::default();

    let results = sequential_checksum::<Md5, _>(inputs, config).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].bytes_processed, 1024);

    let expected = Md5::digest(&data);
    assert_eq!(results[0].digest.as_ref(), expected.as_ref());
}

#[test]
fn test_sequential_checksum_multiple_files() {
    let inputs = vec![
        ChecksumInput::new(Cursor::new(vec![0xAA; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0xBB; 1024]), 1024),
        ChecksumInput::new(Cursor::new(vec![0xCC; 256]), 256),
    ];
    let config = PipelineConfig::default();

    let results = sequential_checksum::<Sha256, _>(inputs, config).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].bytes_processed, 512);
    assert_eq!(results[1].bytes_processed, 1024);
    assert_eq!(results[2].bytes_processed, 256);

    let expected0 = Sha256::digest(&vec![0xAA; 512]);
    let expected1 = Sha256::digest(&vec![0xBB; 1024]);
    let expected2 = Sha256::digest(&vec![0xCC; 256]);

    assert_eq!(results[0].digest.as_ref(), expected0.as_ref());
    assert_eq!(results[1].digest.as_ref(), expected1.as_ref());
    assert_eq!(results[2].digest.as_ref(), expected2.as_ref());
}

#[test]
fn test_pipelined_checksum_empty_input() {
    let inputs: Vec<ChecksumInput<Cursor<Vec<u8>>>> = vec![];
    let config = PipelineConfig::default();

    let results = pipelined_checksum::<Md5, _>(inputs, config).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_pipelined_checksum_single_file() {
    let data = vec![0x55; 2048];
    let inputs = vec![ChecksumInput::new(Cursor::new(data.clone()), 2048)];
    let config = PipelineConfig::default();

    let results = pipelined_checksum::<Md5, _>(inputs, config).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].bytes_processed, 2048);

    let expected = Md5::digest(&data);
    assert_eq!(results[0].digest.as_ref(), expected.as_ref());
}

#[test]
fn test_pipelined_checksum_multiple_files() {
    let inputs = vec![
        ChecksumInput::new(Cursor::new(vec![0x11; 1024]), 1024),
        ChecksumInput::new(Cursor::new(vec![0x22; 2048]), 2048),
        ChecksumInput::new(Cursor::new(vec![0x33; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0x44; 4096]), 4096),
    ];
    let config = PipelineConfig::default();

    let results = pipelined_checksum::<Sha256, _>(inputs, config).unwrap();
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].bytes_processed, 1024);
    assert_eq!(results[1].bytes_processed, 2048);
    assert_eq!(results[2].bytes_processed, 512);
    assert_eq!(results[3].bytes_processed, 4096);
}

#[test]
fn test_parity_sequential_vs_pipelined() {
    let inputs_seq = vec![
        ChecksumInput::new(Cursor::new(vec![0xAA; 1024]), 1024),
        ChecksumInput::new(Cursor::new(vec![0xBB; 2048]), 2048),
        ChecksumInput::new(Cursor::new(vec![0xCC; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0xDD; 4096]), 4096),
    ];
    let inputs_pipe = vec![
        ChecksumInput::new(Cursor::new(vec![0xAA; 1024]), 1024),
        ChecksumInput::new(Cursor::new(vec![0xBB; 2048]), 2048),
        ChecksumInput::new(Cursor::new(vec![0xCC; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0xDD; 4096]), 4096),
    ];

    let config = PipelineConfig::default();

    let seq_results = sequential_checksum::<Md5, _>(inputs_seq, config).unwrap();
    let pipe_results = pipelined_checksum::<Md5, _>(inputs_pipe, config).unwrap();

    assert_eq!(seq_results.len(), pipe_results.len());
    for (seq, pipe) in seq_results.iter().zip(pipe_results.iter()) {
        assert_eq!(seq.digest.as_ref(), pipe.digest.as_ref());
        assert_eq!(seq.bytes_processed, pipe.bytes_processed);
    }
}

#[test]
fn test_parity_different_algorithms() {
    let data = vec![0x77; 8192];

    // Md5
    let inputs_md5 = vec![ChecksumInput::new(Cursor::new(data.clone()), 8192)];
    let config = PipelineConfig::default();
    let seq_md5 = sequential_checksum::<Md5, _>(inputs_md5, config).unwrap();

    let inputs_md5_pipe = vec![ChecksumInput::new(Cursor::new(data.clone()), 8192)];
    let pipe_md5 = pipelined_checksum::<Md5, _>(inputs_md5_pipe, config).unwrap();
    assert_eq!(seq_md5[0].digest.as_ref(), pipe_md5[0].digest.as_ref());

    // Sha256
    let inputs_sha = vec![ChecksumInput::new(Cursor::new(data.clone()), 8192)];
    let seq_sha = sequential_checksum::<Sha256, _>(inputs_sha, config).unwrap();

    let inputs_sha_pipe = vec![ChecksumInput::new(Cursor::new(data.clone()), 8192)];
    let pipe_sha = pipelined_checksum::<Sha256, _>(inputs_sha_pipe, config).unwrap();
    assert_eq!(seq_sha[0].digest.as_ref(), pipe_sha[0].digest.as_ref());

    // Xxh3
    let inputs_xxh = vec![ChecksumInput::new(Cursor::new(data.clone()), 8192)];
    let seq_xxh = sequential_checksum::<Xxh3, _>(inputs_xxh, config).unwrap();

    let inputs_xxh_pipe = vec![ChecksumInput::new(Cursor::new(data), 8192)];
    let pipe_xxh = pipelined_checksum::<Xxh3, _>(inputs_xxh_pipe, config).unwrap();
    assert_eq!(seq_xxh[0].digest.as_ref(), pipe_xxh[0].digest.as_ref());
}

#[test]
fn test_pipelined_checksum_builder() {
    let processor = PipelinedChecksum::builder()
        .buffer_size(8192)
        .threshold(2)
        .build();

    assert_eq!(processor.buffer_size(), 8192);
    assert_eq!(processor.threshold(), 2);
}

#[test]
fn test_pipelined_checksum_automatic_path_selection() {
    let inputs_small = vec![
        ChecksumInput::new(Cursor::new(vec![0x11; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0x22; 512]), 512),
    ];

    let processor = PipelinedChecksum::builder().threshold(3).build();

    let results = processor.compute::<Md5, _>(inputs_small).unwrap();
    assert_eq!(results.len(), 2);

    let inputs_large = vec![
        ChecksumInput::new(Cursor::new(vec![0x33; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0x44; 512]), 512),
        ChecksumInput::new(Cursor::new(vec![0x55; 512]), 512),
    ];

    let results = processor.compute::<Md5, _>(inputs_large).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_checksum_input_creation() {
    let input1 = ChecksumInput::new(Cursor::new(vec![0u8; 100]), 100);
    assert_eq!(input1.size_hint, Some(100));

    let input2 = ChecksumInput::new(Cursor::new(vec![0u8; 100]), 0);
    assert_eq!(input2.size_hint, None);

    let input3 = ChecksumInput::without_hint(Cursor::new(vec![0u8; 100]));
    assert_eq!(input3.size_hint, None);
}

#[test]
fn test_pipeline_config_builder() {
    let config = PipelineConfig::new()
        .with_buffer_size(16384)
        .with_threshold(8);

    assert_eq!(config.buffer_size, 16384);
    assert_eq!(config.threshold, 8);
}

#[test]
fn test_checksum_result_equality() {
    let result1 = ChecksumResult {
        digest: [0u8; 16],
        bytes_processed: 1024,
    };
    let result2 = ChecksumResult {
        digest: [0u8; 16],
        bytes_processed: 1024,
    };
    let result3 = ChecksumResult {
        digest: [1u8; 16],
        bytes_processed: 1024,
    };

    assert_eq!(result1, result2);
    assert_ne!(result1, result3);
}

#[test]
fn test_large_data_parity() {
    let large_data = vec![0x99; 256 * 1024]; // 256 KB

    let inputs_seq = vec![ChecksumInput::new(
        Cursor::new(large_data.clone()),
        large_data.len() as u64,
    )];
    let inputs_pipe = vec![ChecksumInput::new(
        Cursor::new(large_data.clone()),
        large_data.len() as u64,
    )];

    let config = PipelineConfig::default();

    let seq_results = sequential_checksum::<Sha256, _>(inputs_seq, config).unwrap();
    let pipe_results = pipelined_checksum::<Sha256, _>(inputs_pipe, config).unwrap();

    assert_eq!(
        seq_results[0].digest.as_ref(),
        pipe_results[0].digest.as_ref()
    );
    assert_eq!(seq_results[0].bytes_processed, large_data.len() as u64);
    assert_eq!(pipe_results[0].bytes_processed, large_data.len() as u64);
}

#[test]
fn test_threshold_boundary() {
    let processor = PipelinedChecksum::builder().threshold(4).build();

    let inputs_at_threshold = vec![
        ChecksumInput::new(Cursor::new(vec![0x10; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x20; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x30; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x40; 100]), 100),
    ];

    let results = processor.compute::<Md5, _>(inputs_at_threshold).unwrap();
    assert_eq!(results.len(), 4);

    let inputs_below = vec![
        ChecksumInput::new(Cursor::new(vec![0x10; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x20; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x30; 100]), 100),
    ];

    let results = processor.compute::<Md5, _>(inputs_below).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_empty_file_handling() {
    let inputs = vec![
        ChecksumInput::new(Cursor::new(vec![]), 0),
        ChecksumInput::new(Cursor::new(vec![0xAB; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![]), 0),
    ];

    let config = PipelineConfig::default();

    let seq_results = sequential_checksum::<Md5, _>(inputs, config).unwrap();
    assert_eq!(seq_results.len(), 3);
    assert_eq!(seq_results[0].bytes_processed, 0);
    assert_eq!(seq_results[1].bytes_processed, 100);
    assert_eq!(seq_results[2].bytes_processed, 0);
}

#[test]
fn test_mixed_sizes_parity() {
    let inputs_seq = vec![
        ChecksumInput::new(Cursor::new(vec![0x01; 10]), 10),
        ChecksumInput::new(Cursor::new(vec![0x02; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x03; 1000]), 1000),
        ChecksumInput::new(Cursor::new(vec![0x04; 10000]), 10000),
        ChecksumInput::new(Cursor::new(vec![0x05; 50000]), 50000),
    ];

    let inputs_pipe = vec![
        ChecksumInput::new(Cursor::new(vec![0x01; 10]), 10),
        ChecksumInput::new(Cursor::new(vec![0x02; 100]), 100),
        ChecksumInput::new(Cursor::new(vec![0x03; 1000]), 1000),
        ChecksumInput::new(Cursor::new(vec![0x04; 10000]), 10000),
        ChecksumInput::new(Cursor::new(vec![0x05; 50000]), 50000),
    ];

    let config = PipelineConfig::default();

    let seq_results = sequential_checksum::<Xxh3, _>(inputs_seq, config).unwrap();
    let pipe_results = pipelined_checksum::<Xxh3, _>(inputs_pipe, config).unwrap();

    assert_eq!(seq_results.len(), pipe_results.len());
    for (seq, pipe) in seq_results.iter().zip(pipe_results.iter()) {
        assert_eq!(seq.digest.as_ref(), pipe.digest.as_ref());
        assert_eq!(seq.bytes_processed, pipe.bytes_processed);
    }
}

#[test]
fn test_default_implementations() {
    let config1 = PipelineConfig::default();
    let config2 = PipelineConfig::new();
    assert_eq!(config1.buffer_size, config2.buffer_size);
    assert_eq!(config1.threshold, config2.threshold);

    let processor1 = PipelinedChecksum::default();
    let processor2 = PipelinedChecksum::new();
    assert_eq!(processor1.buffer_size(), processor2.buffer_size());
    assert_eq!(processor1.threshold(), processor2.threshold());

    let builder1 = PipelinedChecksumBuilder::default();
    let builder2 = PipelinedChecksumBuilder::new();
    let p1 = builder1.build();
    let p2 = builder2.build();
    assert_eq!(p1.buffer_size(), p2.buffer_size());
}
