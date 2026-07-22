# signature

File signature layout and generation - computes rsync-compatible block signatures
using rolling and strong checksums, with block sizing from upstream `generator.c`.

## Key Public Types

- `FileSignature` - complete signature for a file (collection of block checksums)
- `SignatureLayout` - computed layout parameters (block size, checksum length, count)
- `SignatureLayoutParams` - input parameters for layout calculation
- `SignatureAlgorithm` - checksum algorithm selection (MD4, MD5, XXH3)
- `SignatureBlock` - individual block's rolling + strong checksum pair

## Key Functions

- `calculate_signature_layout` - determines block size and checksum length from file size
- `generate_file_signature` - reads file blocks and computes all checksums
- `calculate_block_length` - standalone block size calculation (upstream `sum_sizes_sqroot`)
- `calculate_checksum_count` - number of blocks for a given file size

## Modules

- `block_size` - block sizing heuristics matching upstream rsync 3.4.4
- `layout` - signature layout computation
- `generation` - sequential signature generation
- `parallel` - rayon-based parallel signature generation
- `async_gen` - async/pipelined signature generation
- `algorithm` - checksum algorithm selection and dispatch

## Dependencies

- **Upstream:** `checksums` (rolling + strong checksum implementations), `protocol` (version types)
- **Downstream:** `matching` (delta generation), `engine`

## Features

- `parallel` - parallel signature generation (rayon always compiled; alias for compatibility)
- `tracing` - structured logging for performance analysis
