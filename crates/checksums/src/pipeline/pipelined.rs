//! Double-buffered pipelined checksum computation.
//!
//! Spawns an I/O thread that reads chunks and sends them to the compute
//! thread via a channel. The compute thread processes chunks while the
//! I/O thread reads ahead, overlapping computation with I/O.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::strong::StrongDigest;

use super::types::{ChecksumInput, ChecksumResult, PipelineConfig, PipelineMessage};

/// Computes checksums using pipelined double-buffering.
///
/// # Errors
///
/// Returns an error if reading from any input fails.
pub fn pipelined_checksum<D, R>(
    inputs: Vec<ChecksumInput<R>>,
    config: PipelineConfig,
) -> io::Result<Vec<ChecksumResult<D::Digest>>>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read + Send + 'static,
{
    let input_count = inputs.len();
    let (sender, receiver) = mpsc::channel();

    let buffer_size = config.buffer_size;
    let io_thread = thread::spawn(move || {
        io_worker(inputs, buffer_size, sender);
    });

    let results = compute_worker::<D>(receiver, input_count)?;

    io_thread
        .join()
        .map_err(|_| io::Error::other("I/O thread panicked"))?;

    Ok(results)
}

/// I/O worker thread function.
///
/// Reads chunks from inputs and sends them to the compute thread.
/// Implements double-buffering by alternating between two read buffers.
fn io_worker<R: Read>(
    mut inputs: Vec<ChecksumInput<R>>,
    buffer_size: usize,
    sender: Sender<PipelineMessage>,
) {
    for (index, input) in inputs.iter_mut().enumerate() {
        let reader = &mut input.reader;
        let mut buffer_a = vec![0u8; buffer_size];
        let mut buffer_b = vec![0u8; buffer_size];
        let mut use_buffer_a = true;

        loop {
            let buffer = if use_buffer_a {
                &mut buffer_a
            } else {
                &mut buffer_b
            };

            match reader.read(buffer) {
                Ok(0) => {
                    if sender
                        .send(PipelineMessage::InputComplete { input_index: index })
                        .is_err()
                    {
                        return;
                    }
                    break;
                }
                Ok(bytes_read) => {
                    let data = buffer[..bytes_read].to_vec();

                    if sender
                        .send(PipelineMessage::Chunk {
                            input_index: index,
                            data,
                        })
                        .is_err()
                    {
                        return;
                    }

                    use_buffer_a = !use_buffer_a;
                }
                Err(e) => {
                    let _ = sender.send(PipelineMessage::Error(e));
                    return;
                }
            }
        }
    }

    let _ = sender.send(PipelineMessage::AllComplete);
}

/// Compute worker function.
///
/// Receives chunks from the I/O thread and computes checksums.
fn compute_worker<D>(
    receiver: Receiver<PipelineMessage>,
    input_count: usize,
) -> io::Result<Vec<ChecksumResult<D::Digest>>>
where
    D: StrongDigest,
    D::Seed: Default,
{
    let mut results: Vec<Option<ChecksumResult<D::Digest>>> = vec![None; input_count];
    let mut hashers: Vec<D> = (0..input_count).map(|_| D::new()).collect();
    let mut byte_counts: Vec<u64> = vec![0; input_count];

    loop {
        match receiver.recv() {
            Ok(PipelineMessage::Chunk { input_index, data }) => {
                if input_index < input_count {
                    hashers[input_index].update(&data);
                    byte_counts[input_index] += data.len() as u64;
                }
            }
            Ok(PipelineMessage::InputComplete { input_index }) => {
                if input_index < input_count {
                    let hasher = std::mem::replace(&mut hashers[input_index], D::new());
                    let digest = hasher.finalize();
                    results[input_index] = Some(ChecksumResult {
                        digest,
                        bytes_processed: byte_counts[input_index],
                    });
                }
            }
            Ok(PipelineMessage::AllComplete) => {
                break;
            }
            Ok(PipelineMessage::Error(e)) => {
                return Err(e);
            }
            Err(_) => {
                break;
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(i, opt)| {
            opt.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("Input {i} was not completed"),
                )
            })
        })
        .collect()
}
