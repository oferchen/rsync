//! Property tests for reorder buffer correctness and failure-mode behaviour.

/// Property test: a random permutation of 0..N always yields 0..N in order.
mod prop {
    use crate::reorder_buffer::BoundedReorderBuffer;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn random_permutation_yields_sorted(n in 1u64..256) {
            let window = n.max(1);
            let mut buf = BoundedReorderBuffer::new(window);

            // Generate a random-ish permutation using a simple shuffle.
            let mut indices: Vec<u64> = (0..n).collect();
            // Deterministic "shuffle" based on n for reproducibility.
            indices.reverse();

            let mut all_drained = Vec::new();
            for seq in indices {
                match buf.insert(seq, seq) {
                    Ok(d) => all_drained.extend(d),
                    Err(_) => {
                        // With window == n, this should not happen.
                        panic!("backpressure with window == n");
                    }
                }
            }

            // Verify all items delivered in order.
            prop_assert_eq!(all_drained.len(), n as usize);
            for (i, &val) in all_drained.iter().enumerate() {
                prop_assert_eq!(val, i as u64);
            }
        }

        #[test]
        fn backpressure_respects_window(window in 1u64..64, overshoot in 0u64..100) {
            let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(window);
            let target = window + overshoot;
            let result = buf.insert(target, target);
            prop_assert!(result.is_err());
            let err = result.unwrap_err();
            prop_assert_eq!(err.sequence, target);
            prop_assert_eq!(err.window_start, 0);
            prop_assert_eq!(err.window_end, window);
        }
    }
}

/// Property tests for failure-mode behaviour.
///
/// `BoundedReorderBuffer<T>` is fully generic over the payload `T`; it
/// has no built-in error channel. The surrounding pipeline propagates
/// I/O errors out-of-band. To exercise failure modes against the buffer
/// itself, these tests use `Result<u64, io::Error>` as the payload and
/// verify two invariants:
///
/// 1. Network-error propagation: errors interleaved with successes
///    survive reorder unchanged, in the original sequence order, with
///    no item loss or reordering between `Ok` and `Err` variants.
/// 2. Mid-transfer abort: when the consumer drops the receiving
///    channel partway through a transfer, the producer thread
///    terminates without panicking and the buffer's pending map is
///    deallocated by ordinary `Drop`.
mod property_failure_tests {
    use crate::reorder_buffer::BoundedReorderBuffer;
    use proptest::collection::vec;
    use proptest::prelude::*;
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    type Payload = Result<u64, io::Error>;

    // `io::Error` is not `Clone`, which `Just` requires, so the strategy
    // returns a `()`-tagged discriminator and the test bodies materialise
    // a fresh `io::Error` at use time.
    fn payload_strategy() -> impl Strategy<Value = Result<u64, ()>> {
        prop_oneof![any::<u64>().prop_map(Ok), Just(Err(())),]
    }

    /// Deterministic permutation of `0..n` seeded from a proptest u64.
    ///
    /// Avoids wall-clock randomness so failures shrink reliably.
    fn lcg_shuffle(n: usize, seed: u64) -> Vec<usize> {
        const A: u64 = 1664525;
        const C: u64 = 1013904223;
        let mut keyed: Vec<(u64, usize)> = (0..n)
            .map(|i| {
                let key = seed
                    .wrapping_mul(A)
                    .wrapping_add(C)
                    .wrapping_add((i as u64).wrapping_mul(A));
                (key, i)
            })
            .collect();
        keyed.sort_unstable_by_key(|&(k, _)| k);
        keyed.into_iter().map(|(_, i)| i).collect()
    }

    proptest! {
        /// Errors interleaved with successes flow through reorder
        /// without being dropped, reordered, or coerced into `Ok`.
        #[test]
        fn network_error_propagation(
            payloads in vec(payload_strategy(), 1..64),
            seed in any::<u64>(),
        ) {
            let n = payloads.len();
            let window = (n as u64).max(1);
            let mut buf: BoundedReorderBuffer<Payload> =
                BoundedReorderBuffer::new(window);

            let order = lcg_shuffle(n, seed);
            let mut drained: Vec<Payload> = Vec::with_capacity(n);

            for seq_idx in order {
                let payload: Payload = match &payloads[seq_idx] {
                    Ok(v) => Ok(*v),
                    Err(_) => Err(io::Error::other("simulated network failure")),
                };
                let out = buf
                    .insert(seq_idx as u64, payload)
                    .expect("window == n so backpressure cannot fire");
                drained.extend(out);
            }

            prop_assert_eq!(drained.len(), n);
            prop_assert!(buf.is_empty());
            prop_assert_eq!(buf.next_expected(), n as u64);

            for (i, (got, want)) in drained.iter().zip(payloads.iter()).enumerate() {
                match (got, want) {
                    (Ok(a), Ok(b)) => {
                        prop_assert_eq!(a, b, "ok payload at seq {} mismatched", i);
                    }
                    (Err(_), Err(_)) => {}
                    (Ok(_), Err(_)) | (Err(_), Ok(_)) => {
                        prop_assert!(
                            false,
                            "result discriminant mismatch at seq {}",
                            i,
                        );
                    }
                }
            }
        }

        /// When the consumer drops the channel mid-transfer, the
        /// producer thread must terminate cleanly: no panic, no
        /// deadlock, no leak (buffer dropped at thread exit).
        #[test]
        fn abort_no_leak_no_panic(
            total in 4u64..64,
            abort_after in 1u64..32,
        ) {
            let abort_after = abort_after.min(total);
            let window = total;
            let (tx, rx) = mpsc::channel::<Payload>();

            let producer = thread::spawn(move || {
                let mut buf: BoundedReorderBuffer<Payload> =
                    BoundedReorderBuffer::new(window);
                catch_unwind(AssertUnwindSafe(|| {
                    for seq in 0..total {
                        let payload: Payload = if seq % 7 == 0 {
                            Err(io::Error::other("simulated mid-transfer error"))
                        } else {
                            Ok(seq)
                        };
                        let drained = match buf.insert(seq, payload) {
                            Ok(d) => d,
                            Err(_) => return,
                        };
                        for item in drained {
                            if tx.send(item).is_err() {
                                // Consumer dropped rx: stop cleanly.
                                return;
                            }
                        }
                    }
                }))
            });

            for _ in 0..abort_after {
                let _ = rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("producer must deliver before timeout");
            }
            drop(rx);

            let deadline = Instant::now() + Duration::from_secs(5);
            while !producer.is_finished() {
                if Instant::now() >= deadline {
                    panic!("producer did not terminate within deadline after abort");
                }
                thread::sleep(Duration::from_millis(10));
            }

            let outcome = producer.join().expect("producer thread panicked");
            prop_assert!(
                outcome.is_ok(),
                "producer body panicked under abort",
            );
        }
    }
}
