//! Unit tests for the sequence-based reorder buffer.

mod core_tests {
    use super::super::*;

    #[test]
    fn in_order_delivery() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();

        assert_eq!(buf.next_in_order(), Some("a"));
        assert_eq!(buf.next_in_order(), Some("b"));
        assert_eq!(buf.next_in_order(), Some("c"));
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    fn out_of_order_reordering() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(2, "c").unwrap();
        assert_eq!(buf.next_in_order(), None); // waiting for 0

        buf.insert(0, "a").unwrap();
        assert_eq!(buf.next_in_order(), Some("a"));
        assert_eq!(buf.next_in_order(), None); // waiting for 1

        buf.insert(1, "b").unwrap();
        assert_eq!(buf.next_in_order(), Some("b"));
        assert_eq!(buf.next_in_order(), Some("c"));
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    fn gap_handling() {
        let mut buf = ReorderBuffer::new(16);
        // Insert 0, 2, 4 - gaps at 1 and 3
        buf.insert(0, 'a').unwrap();
        buf.insert(2, 'c').unwrap();
        buf.insert(4, 'e').unwrap();

        assert_eq!(buf.next_in_order(), Some('a'));
        // Stuck at 1
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 2);

        // Fill gap at 1
        buf.insert(1, 'b').unwrap();
        assert_eq!(buf.next_in_order(), Some('b'));
        assert_eq!(buf.next_in_order(), Some('c'));
        // Stuck at 3
        assert_eq!(buf.next_in_order(), None);

        // Fill gap at 3
        buf.insert(3, 'd').unwrap();
        assert_eq!(buf.next_in_order(), Some('d'));
        assert_eq!(buf.next_in_order(), Some('e'));
        assert_eq!(buf.next_in_order(), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn capacity_bounds_enforcement() {
        let mut buf = ReorderBuffer::new(2);
        // With capacity 2, valid offsets from next_expected (0) are 0 and 1
        buf.insert(0, "x").unwrap();
        buf.insert(1, "y").unwrap();
        // Seq 2 has offset 2 from next_expected=0, which equals capacity
        assert_eq!(buf.insert(2, "z"), Err(CapacityExceeded));
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn capacity_frees_after_drain() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(0, 10).unwrap();
        buf.insert(1, 20).unwrap();
        // Seq 2 has offset 2 from next_expected=0, which equals capacity
        assert_eq!(buf.insert(2, 30), Err(CapacityExceeded));

        assert_eq!(buf.next_in_order(), Some(10));
        // Now there is room (next_expected=1, seq 2 has offset 1)
        buf.insert(2, 30).unwrap();
        assert_eq!(buf.next_in_order(), Some(20));
        assert_eq!(buf.next_in_order(), Some(30));
    }

    #[test]
    fn empty_buffer_behavior() {
        let buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);
        assert_eq!(buf.next_expected(), 0);
        assert_eq!(buf.capacity(), 4);
    }

    #[test]
    fn empty_buffer_next_returns_none() {
        let mut buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _: ReorderBuffer<i32> = ReorderBuffer::new(0);
    }

    #[test]
    fn drain_ready_yields_contiguous_run() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, 'a').unwrap();
        buf.insert(1, 'b').unwrap();
        buf.insert(2, 'c').unwrap();
        buf.insert(4, 'e').unwrap(); // gap at 3

        let drained: Vec<char> = buf.drain_ready().collect();
        assert_eq!(drained, vec!['a', 'b', 'c']);
        assert_eq!(buf.next_expected(), 3);
        assert_eq!(buf.buffered_count(), 1); // 'e' still waiting
    }

    #[test]
    fn drain_ready_empty_buffer() {
        let mut buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert!(drained.is_empty());
    }

    #[test]
    fn drain_ready_no_contiguous() {
        let mut buf = ReorderBuffer::new(4);
        buf.insert(3, "far").unwrap();
        let drained: Vec<&str> = buf.drain_ready().collect();
        assert!(drained.is_empty());
        assert_eq!(buf.buffered_count(), 1);
    }

    #[test]
    fn large_sequence_numbers() {
        let mut buf = ReorderBuffer::new(4);
        let base = u64::MAX - 3;
        // Offset from next_expected (0) is enormous - must be rejected
        assert_eq!(buf.insert(base, "a"), Err(CapacityExceeded));
        assert_eq!(buf.buffered_count(), 0);
    }

    #[test]
    fn next_expected_advances_correctly() {
        let mut buf = ReorderBuffer::new(8);
        assert_eq!(buf.next_expected(), 0);

        buf.insert(0, "x").unwrap();
        let _ = buf.next_in_order();
        assert_eq!(buf.next_expected(), 1);

        buf.insert(1, "y").unwrap();
        buf.insert(2, "z").unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        assert_eq!(buf.next_expected(), 3);
    }

    #[test]
    fn capacity_exceeded_display() {
        assert_eq!(
            CapacityExceeded.to_string(),
            "reorder buffer capacity exceeded"
        );
    }

    #[test]
    fn interleaved_insert_and_drain() {
        let mut buf = ReorderBuffer::new(4);

        // Round 1: insert 0, 2 - drain yields 0
        buf.insert(0, 0).unwrap();
        buf.insert(2, 2).unwrap();
        assert_eq!(buf.next_in_order(), Some(0));
        assert_eq!(buf.next_in_order(), None);

        // Round 2: insert 1 - drain yields 1, 2
        buf.insert(1, 1).unwrap();
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert_eq!(drained, vec![1, 2]);

        // Round 3: insert 3, 4 in order
        buf.insert(3, 3).unwrap();
        buf.insert(4, 4).unwrap();
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert_eq!(drained, vec![3, 4]);
        assert!(buf.is_empty());
    }

    #[test]
    fn duplicate_sequence_overwrites_previous() {
        // Ring buffer replaces the value for an existing slot.
        // This is graceful - no panic, no error - but the original item is lost.
        let mut buf = ReorderBuffer::new(4);
        buf.insert(0, "first").unwrap();
        buf.insert(0, "replaced").unwrap();
        assert_eq!(buf.next_in_order(), Some("replaced"));
        assert_eq!(buf.next_in_order(), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_item() {
        let mut buf = ReorderBuffer::new(1);
        buf.insert(0, 42).unwrap();
        assert_eq!(buf.buffered_count(), 1);
        assert!(!buf.is_empty());
        assert_eq!(buf.next_in_order(), Some(42));
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 1);
    }

    #[test]
    fn large_gap_many_buffered() {
        let mut buf = ReorderBuffer::new(128);
        // Insert sequences 1..=100, leaving gap at 0.
        for i in 1..=100 {
            buf.insert(i, i).unwrap();
        }
        assert_eq!(buf.buffered_count(), 100);
        // Nothing drains - all waiting for seq 0.
        assert_eq!(buf.next_in_order(), None);

        // Fill the gap.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 101);
        assert_eq!(drained[0], 0);
        assert_eq!(drained[100], 100);
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 101);
    }

    #[test]
    fn reverse_order_insertion() {
        let mut buf = ReorderBuffer::new(8);
        for i in (0..5).rev() {
            buf.insert(i, i).unwrap();
        }
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn take_extracts_without_advancing_cursor() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        buf.insert(4, "e").unwrap();

        // Take seq 2 - cursor stays at 0.
        let taken = buf.take(2);
        assert_eq!(taken, Some("c"));
        assert_eq!(buf.next_expected(), 0);
        assert_eq!(buf.buffered_count(), 3);

        // Take seq 4.
        let taken = buf.take(4);
        assert_eq!(taken, Some("e"));
        assert_eq!(buf.buffered_count(), 2);

        // Take non-existent seq.
        assert!(buf.take(3).is_none());
        assert!(buf.take(99).is_none());

        // Remaining items drain in order.
        let drained: Vec<&str> = buf.drain_ready().collect();
        assert_eq!(drained, vec!["a", "b"]);
        // Seq 2 was taken - gap prevents further drain.
        assert_eq!(buf.next_expected(), 2);
    }

    #[test]
    fn force_insert_bypasses_capacity() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(1, "a").unwrap();
        buf.insert(0, "b").unwrap();
        // Normal insert for seq 2 fails (offset 2 >= capacity 2).
        assert_eq!(buf.insert(2, "c"), Err(CapacityExceeded));
        // force_insert grows the ring to accommodate.
        buf.force_insert(2, "c");
        assert_eq!(buf.buffered_count(), 3);
        // drain_ready yields all three in order.
        let drained: Vec<&str> = buf.drain_ready().collect();
        assert_eq!(drained, vec!["b", "a", "c"]);
    }

    /// Verifies that concurrent producers submitting results out of order
    /// still yield strictly ascending sequence delivery to the consumer.
    ///
    /// Simulates the concurrent delta pipeline scenario: multiple worker
    /// threads produce results with known sequence numbers at variable rates.
    /// A single consumer thread owns the `ReorderBuffer` and receives items
    /// via a channel, inserting them and draining in-order results.
    ///
    /// The bounded capacity is exercised: when the buffer is full, the
    /// consumer must drain before accepting more items.
    #[test]
    fn concurrent_producers_in_order_delivery() {
        use std::sync::mpsc;
        use std::thread;

        const TOTAL_ITEMS: u64 = 200;
        const NUM_PRODUCERS: u64 = 4;
        const BUFFER_CAPACITY: usize = 32;

        let (tx, rx) = mpsc::channel::<(u64, u64)>();

        // Spawn producer threads - each owns a disjoint set of sequence numbers.
        let producers: Vec<_> = (0..NUM_PRODUCERS)
            .map(|producer_id| {
                let tx = tx.clone();
                thread::spawn(move || {
                    let mut seq = producer_id;
                    while seq < TOTAL_ITEMS {
                        // Simulate variable work duration via lightweight spin.
                        // Deterministic delay based on sequence to create reordering.
                        let spins = ((seq * 7 + producer_id * 13) % 100) as u32;
                        for _ in 0..spins {
                            std::hint::spin_loop();
                        }

                        tx.send((seq, seq)).unwrap();
                        seq += NUM_PRODUCERS;
                    }
                })
            })
            .collect();

        // Drop the original sender so rx terminates when producers finish.
        drop(tx);

        // Consumer owns the buffer - no shared-mutable-state deadlock.
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(BUFFER_CAPACITY);
        let mut collected: Vec<u64> = Vec::with_capacity(TOTAL_ITEMS as usize);
        let mut capacity_pressure_observed = false;

        for (seq, val) in rx {
            // Try normal insert; on capacity exceeded, drain first then force.
            match buf.insert(seq, val) {
                Ok(()) => {}
                Err(CapacityExceeded) => {
                    capacity_pressure_observed = true;
                    // Drain what we can, then force-insert the item.
                    collected.extend(buf.drain_ready());
                    buf.force_insert(seq, val);
                }
            }
            // Opportunistically drain ready items.
            collected.extend(buf.drain_ready());
        }

        for p in producers {
            p.join().expect("producer panicked");
        }

        collected.extend(buf.drain_ready());

        assert_eq!(
            collected.len(),
            TOTAL_ITEMS as usize,
            "expected {TOTAL_ITEMS} items but got {}",
            collected.len()
        );

        // Verify strictly ascending sequence order.
        for (i, &val) in collected.iter().enumerate() {
            assert_eq!(
                val, i as u64,
                "expected sequence {i} but got {val} - ordering violated"
            );
        }

        // With 4 producers racing and capacity 32, we expect the buffer to
        // have been pressured at least once during the run.
        assert!(
            capacity_pressure_observed,
            "capacity backpressure was never triggered - increase TOTAL_ITEMS or decrease BUFFER_CAPACITY"
        );
    }

    #[test]
    fn finish_succeeds_when_fully_drained() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        buf.finish(); // no panic - all items delivered
    }

    #[test]
    fn finish_succeeds_on_empty_buffer() {
        let buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        buf.finish(); // no items were ever inserted, no gap
    }

    #[test]
    #[should_panic(expected = "sequence gap detected")]
    fn finish_panics_on_gap() {
        let mut buf = ReorderBuffer::new(8);
        // Insert seq 0 and seq 2, skip seq 1 entirely.
        buf.insert(0, "a").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect(); // delivers only seq 0
        // Finishing with seq 1 missing triggers the panic.
        buf.finish();
    }

    #[test]
    #[should_panic(expected = "sequence gap detected")]
    fn finish_panics_when_first_item_missing() {
        let mut buf = ReorderBuffer::new(8);
        // Only insert seq 1 and 2, never seq 0.
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect(); // delivers nothing
        buf.finish();
    }

    /// Validates `ReorderBuffer` with the actual `DeltaResult` type to ensure
    /// the pipeline integration works end-to-end.
    ///
    /// Mirrors upstream `recv_files()` in `receiver.c` where results must be
    /// processed in file-list order regardless of worker completion order.
    #[test]
    fn delta_result_integration() {
        use crate::concurrent_delta::types::DeltaResult;

        let mut buf: ReorderBuffer<DeltaResult> = ReorderBuffer::new(16);

        // Simulate three workers completing out of order: seq 2, 0, 1
        let r2 = DeltaResult::success(20, 2000, 500, 1500).with_sequence(2);
        let r0 = DeltaResult::success(10, 1000, 300, 700).with_sequence(0);
        let r1 = DeltaResult::needs_redo(15, "checksum mismatch".to_string()).with_sequence(1);

        buf.insert(r2.sequence(), r2).unwrap();
        buf.insert(r0.sequence(), r0).unwrap();
        buf.insert(r1.sequence(), r1).unwrap();

        let drained: Vec<DeltaResult> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 3);

        // Verify ordering by sequence.
        assert_eq!(drained[0].sequence(), 0);
        assert_eq!(drained[0].ndx().get(), 10);
        assert!(drained[0].is_success());

        assert_eq!(drained[1].sequence(), 1);
        assert_eq!(drained[1].ndx().get(), 15);
        assert!(drained[1].needs_retry());

        assert_eq!(drained[2].sequence(), 2);
        assert_eq!(drained[2].ndx().get(), 20);
        assert!(drained[2].is_success());
        assert_eq!(drained[2].bytes_written(), 2000);
    }

    #[test]
    fn ring_buffer_wraps_correctly() {
        // Verify head pointer wraps around the ring buffer.
        let mut buf = ReorderBuffer::new(4);

        // Fill and drain twice to force head wrapping.
        for batch in 0..3u64 {
            let base = batch * 4;
            for i in 0..4 {
                buf.insert(base + i, base + i).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            assert_eq!(drained.len(), 4);
            for (j, &val) in drained.iter().enumerate() {
                assert_eq!(val, base + j as u64);
            }
            assert!(buf.is_empty());
        }
        assert_eq!(buf.next_expected(), 12);
    }

    #[test]
    fn ring_buffer_stress_sequential() {
        // Stress test: many sequential insert-drain cycles.
        let mut buf = ReorderBuffer::new(8);
        for i in 0..1000u64 {
            buf.insert(i, i).unwrap();
            assert_eq!(buf.next_in_order(), Some(i));
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 1000);
    }

    #[test]
    fn interleaved_gaps_progressive_fill() {
        // Insert even-numbered items, then progressively fill odd gaps.
        // Each odd fill should cascade delivery through the next even.
        let mut buf = ReorderBuffer::new(16);

        // Insert 0, 2, 4, 6, 8 (gaps at 1, 3, 5, 7).
        for i in (0..10).step_by(2) {
            buf.insert(i, i).unwrap();
        }

        // Only seq 0 is deliverable.
        assert_eq!(buf.next_in_order(), Some(0));
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 4); // 2, 4, 6, 8

        // Fill gap at 1 - should cascade to deliver 1, 2.
        buf.insert(1, 1).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![1, 2]);
        assert_eq!(buf.next_expected(), 3);

        // Fill gap at 3 - cascades 3, 4.
        buf.insert(3, 3).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![3, 4]);

        // Fill gap at 5 - cascades 5, 6.
        buf.insert(5, 5).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![5, 6]);

        // Fill gap at 7 - cascades 7, 8.
        buf.insert(7, 7).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![7, 8]);

        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 9);
    }

    #[test]
    fn burst_after_gap() {
        // Insert a contiguous burst 0-4, then a second burst 10-14 with a gap
        // at 5-9. Verify first burst drains, second is stuck until gap fills.
        let mut buf = ReorderBuffer::new(32);

        for i in 0..5 {
            buf.insert(i, i).unwrap();
        }
        for i in 10..15 {
            buf.insert(i, i).unwrap();
        }

        // Drain the first burst.
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
        assert_eq!(buf.next_expected(), 5);
        assert_eq!(buf.buffered_count(), 5); // 10-14 waiting

        // Nothing drains while 5-9 are missing.
        assert_eq!(buf.next_in_order(), None);

        // Fill the gap 5-9.
        for i in 5..10 {
            buf.insert(i, i).unwrap();
        }

        // Now 5-14 should all drain in order.
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![5, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 15);
    }

    /// Deterministic pseudo-random permutation stress test.
    ///
    /// Inserts 1000 items in a deterministic shuffled order using a simple
    /// linear congruential generator. Verifies the output is perfectly
    /// ordered 0-999 regardless of insertion order.
    #[test]
    fn stress_deterministic_random_order() {
        const N: usize = 1000;
        let capacity = N;
        let mut buf = ReorderBuffer::new(capacity);

        // Generate a deterministic permutation of 0..N using Fisher-Yates
        // with a simple LCG for reproducibility.
        let mut perm: Vec<u64> = (0..N as u64).collect();
        let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_1234; // fixed seed
        for i in (1..N).rev() {
            // LCG: state = state * 6364136223846793005 + 1442695040888963407
            rng_state = rng_state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = (rng_state >> 33) as usize % (i + 1);
            perm.swap(i, j);
        }

        let mut collected: Vec<u64> = Vec::with_capacity(N);
        for &seq in &perm {
            buf.insert(seq, seq).unwrap();
            collected.extend(buf.drain_ready());
        }
        collected.extend(buf.drain_ready());

        assert_eq!(collected.len(), N);
        for (i, &val) in collected.iter().enumerate() {
            assert_eq!(
                val, i as u64,
                "expected sequence {i} but got {val} at output position {i}"
            );
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), N as u64);
    }

    #[test]
    fn large_gap_fill_one_at_a_time() {
        // Insert item 100 first, then fill 0-99 one at a time in forward order.
        // Nothing should be delivered until seq 0 arrives, then cascading delivery.
        let mut buf = ReorderBuffer::new(128);

        buf.insert(100, 100u64).unwrap();
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 1);

        // Fill 1-99 (still missing seq 0).
        for i in 1..100 {
            buf.insert(i, i).unwrap();
            assert_eq!(
                buf.next_in_order(),
                None,
                "should not deliver before seq 0 arrives (inserting {i})"
            );
        }
        assert_eq!(buf.buffered_count(), 100);

        // Insert seq 0 - triggers cascade of all 101 items.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 101);
        for (i, &val) in drained.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 101);
    }

    #[test]
    fn force_insert_beyond_capacity_then_drain() {
        // Verify force_insert with a large gap grows the buffer and maintains
        // ordering after the gap is filled.
        let mut buf = ReorderBuffer::new(4);

        buf.insert(0, 0u64).unwrap();
        buf.insert(1, 1).unwrap();

        // Force-insert far beyond capacity.
        buf.force_insert(20, 20);
        assert!(buf.capacity() > 4); // ring was grown

        // Fill the gap 2-19.
        for i in 2..20 {
            buf.insert(i, i).unwrap();
        }

        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 21);
        for (i, &val) in drained.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn multiple_drain_ready_calls_are_idempotent() {
        // After drain_ready exhausts contiguous items, subsequent calls
        // yield nothing until a new contiguous item is inserted.
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, 'a').unwrap();
        buf.insert(1, 'b').unwrap();
        buf.insert(3, 'd').unwrap(); // gap at 2

        let first: Vec<char> = buf.drain_ready().collect();
        assert_eq!(first, vec!['a', 'b']);

        let second: Vec<char> = buf.drain_ready().collect();
        assert!(second.is_empty());

        // Fill the gap.
        buf.insert(2, 'c').unwrap();
        let third: Vec<char> = buf.drain_ready().collect();
        assert_eq!(third, vec!['c', 'd']);
    }

    /// Verifies that `ReorderBuffer` produces strictly sequential output
    /// regardless of insertion order.
    ///
    /// This is the channel-agnostic ordering invariant: the reorder buffer
    /// restores submission order using sequence numbers, independent of the
    /// underlying channel implementation (std mpsc, crossbeam, etc.).
    ///
    /// Tests three scenarios:
    /// 1. A small batch inserted in a specific scrambled order.
    /// 2. A large batch (200 items) inserted in a deterministic pseudo-random
    ///    order derived from a fixed seed.
    /// 3. Burst insertion (groups of items arrive together) with interleaved drains.
    #[test]
    fn reorder_ordering_invariant() {
        // Scenario 1: Small batch, specific scrambled order.
        {
            let mut buf = ReorderBuffer::new(16);
            let insertion_order: Vec<u64> = vec![5, 2, 0, 3, 1, 4];
            for seq in &insertion_order {
                buf.insert(*seq, *seq).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            let expected: Vec<u64> = (0..6).collect();
            assert_eq!(
                drained, expected,
                "small batch: output must be sequential 0..6"
            );
            assert!(buf.is_empty());
        }

        // Scenario 2: Large batch with deterministic pseudo-random insertion order.
        // Uses a simple LCG (linear congruential generator) seeded at 42 to
        // produce a fixed permutation of 0..200, ensuring determinism across runs.
        {
            let n: u64 = 200;
            let mut indices: Vec<u64> = (0..n).collect();

            // Fisher-Yates shuffle with deterministic LCG.
            let mut rng_state: u64 = 42;
            for i in (1..indices.len()).rev() {
                // LCG: state = state * 6364136223846793005 + 1 (mod 2^64)
                rng_state = rng_state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let j = (rng_state >> 33) as usize % (i + 1);
                indices.swap(i, j);
            }

            let mut buf = ReorderBuffer::new(n as usize);
            for &seq in &indices {
                buf.insert(seq, seq * 10).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            let expected: Vec<u64> = (0..n).map(|i| i * 10).collect();
            assert_eq!(
                drained, expected,
                "large batch: output must be sequential with correct values"
            );
            assert!(buf.is_empty());
        }

        // Scenario 3: Burst insertion with interleaved drains.
        // Items arrive in bursts (groups of 5), each burst scrambled,
        // with drains after each burst.
        {
            let total: u64 = 30;
            let burst_size: u64 = 5;
            let mut buf = ReorderBuffer::new(total as usize);
            let mut collected = Vec::new();

            for burst_start in (0..total).step_by(burst_size as usize) {
                // Each burst arrives in reverse order within the group.
                let burst_end = (burst_start + burst_size).min(total);
                for seq in (burst_start..burst_end).rev() {
                    buf.insert(seq, seq).unwrap();
                }
                // Drain whatever is ready after this burst.
                collected.extend(buf.drain_ready());
            }

            let expected: Vec<u64> = (0..total).collect();
            assert_eq!(
                collected, expected,
                "burst insertion: output must be sequential 0..{total}"
            );
            assert!(buf.is_empty());
        }
    }
}

mod adaptive_tests {
    use super::super::*;

    /// Default-constructed buffers must remain unaffected by adaptive logic.
    #[test]
    fn fixed_capacity_default_unchanged() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(4);
        for i in 0..4 {
            buf.insert(i, i).unwrap();
        }
        assert_eq!(buf.capacity(), 4);
        let stats = buf.stats();
        assert_eq!(stats.grow_events, 0);
        assert_eq!(stats.shrink_events, 0);
        assert_eq!(stats.capacity, 4);
        // Capacity-exceeded behaviour preserved.
        assert_eq!(buf.insert(4, 4), Err(CapacityExceeded));
    }

    #[test]
    fn adaptive_buffer_starts_at_min_capacity() {
        let policy = AdaptiveCapacityPolicy::new(4, 32, 2.0);
        let buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);
        assert_eq!(buf.capacity(), 4);
        assert_eq!(buf.stats().grow_events, 0);
    }

    /// Inserting beyond `min` capacity grows the ring (without losing items)
    /// up to but never beyond `max`.
    #[test]
    fn grows_under_load() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 16, 2.0, 64);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Build a wide gap (insert seq 0..7 with a hole at 0) to force growth.
        for seq in 1..8 {
            // seq 1, 2 fit (capacity 2 -> grows). Subsequent inserts continue
            // to grow up to max as the gap window widens.
            buf.insert(seq, seq).unwrap();
        }
        let stats = buf.stats();
        assert!(stats.grow_events > 0, "buffer never grew under load");
        assert!(
            buf.capacity() >= 8,
            "capacity {} < required gap window 8",
            buf.capacity()
        );
        assert!(
            buf.capacity() <= 16,
            "capacity {} exceeded max",
            buf.capacity()
        );

        // Fill the head and confirm ordered drain still works after growth.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, (0..8).collect::<Vec<_>>());
    }

    /// The grow path must never breach `policy.max`.
    #[test]
    fn never_exceeds_max() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 8, 2.0, 64);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Try to drive a gap of 12 - capacity should cap at 8.
        for seq in 1..8 {
            buf.insert(seq, seq).unwrap();
        }
        // Once at max, inserts beyond capacity must error.
        let err = buf.insert(8, 8);
        assert_eq!(err, Err(CapacityExceeded));
        assert!(buf.capacity() <= 8);
    }

    /// After a sustained idle window, capacity should shrink back toward `min`.
    #[test]
    fn shrinks_when_idle() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 32, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Force a grow: build a gap so capacity expands.
        for seq in 1..6 {
            buf.insert(seq, seq).unwrap();
        }
        let grown = buf.capacity();
        assert!(grown >= 6, "expected grown capacity, got {grown}");
        let grow_events = buf.stats().grow_events;
        assert!(grow_events >= 1);

        // Drain everything to clear the gap.
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        assert!(buf.is_empty());

        // Submit single-item inserts that immediately drain - low utilization.
        for seq in 6..30 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        let stats = buf.stats();
        assert!(stats.shrink_events >= 1, "buffer never shrank when idle");
        assert!(buf.capacity() < grown, "capacity did not decrease");
    }

    /// Shrinks must clamp at `policy.min` no matter how long the idle window.
    #[test]
    fn never_drops_below_min() {
        let min = 4usize;
        let policy = AdaptiveCapacityPolicy::with_window(min, 32, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Drive growth then quiesce.
        for seq in 1..10 {
            buf.insert(seq, seq).unwrap();
        }
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();

        for seq in 10..200 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        assert!(
            buf.capacity() >= min,
            "capacity {} dropped below min {min}",
            buf.capacity()
        );
        // Min is the hard floor.
        assert_eq!(buf.capacity(), min);
    }

    /// Stats reflect both grow and shrink events accurately.
    #[test]
    fn stats_track_both_events() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 16, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Trigger at least one grow.
        for seq in 1..6 {
            buf.insert(seq, seq).unwrap();
        }
        assert!(buf.stats().grow_events >= 1);

        // Drain and idle to trigger at least one shrink.
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        for seq in 6..40 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        let stats = buf.stats();
        assert!(stats.grow_events >= 1);
        assert!(stats.shrink_events >= 1);
        assert_eq!(stats.capacity, buf.capacity());
    }

    /// Ordering is preserved across grow / shrink transitions.
    #[test]
    fn ordering_preserved_through_resize() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 64, 2.0, 8);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        let n: u64 = 200;
        // Insert in reversed bursts of 8 to force out-of-order delivery and
        // exercise both grow and shrink paths.
        let mut collected: Vec<u64> = Vec::with_capacity(n as usize);
        for burst_start in (0..n).step_by(8) {
            let end = (burst_start + 8).min(n);
            for seq in (burst_start..end).rev() {
                buf.insert(seq, seq).unwrap();
            }
            collected.extend(buf.drain_ready());
        }
        let expected: Vec<u64> = (0..n).collect();
        assert_eq!(collected, expected);
    }
}

mod passthrough_tests {
    use super::super::*;

    #[test]
    fn passthrough_delivers_in_insertion_order() {
        let mut buf: ReorderBuffer<&str> = ReorderBuffer::passthrough();
        assert!(buf.is_passthrough());

        // Insert out of sequence order.
        buf.insert(2, "third").unwrap();
        buf.insert(0, "first").unwrap();
        buf.insert(1, "second").unwrap();

        // Items come out in insertion order, not sequence order.
        assert_eq!(buf.next_in_order(), Some("third"));
        assert_eq!(buf.next_in_order(), Some("first"));
        assert_eq!(buf.next_in_order(), Some("second"));
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    fn passthrough_insert_never_returns_capacity_exceeded() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        // Even very large sequence numbers succeed - no ring buffer bounds.
        assert!(buf.insert(u64::MAX - 1, 999).is_ok());
        assert!(buf.insert(0, 0).is_ok());
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn passthrough_drain_ready_yields_all_items() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        buf.insert(5, 50).unwrap();
        buf.insert(3, 30).unwrap();
        buf.insert(1, 10).unwrap();

        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![50, 30, 10]);
        assert!(buf.is_empty());
    }

    #[test]
    fn passthrough_force_insert_appends_to_queue() {
        let mut buf: ReorderBuffer<&str> = ReorderBuffer::passthrough();
        buf.force_insert(10, "forced");
        buf.force_insert(5, "also_forced");

        assert_eq!(buf.buffered_count(), 2);
        assert_eq!(buf.next_in_order(), Some("forced"));
        assert_eq!(buf.next_in_order(), Some("also_forced"));
    }

    #[test]
    fn passthrough_is_empty_and_count() {
        let mut buf: ReorderBuffer<i32> = ReorderBuffer::passthrough();
        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);

        buf.insert(0, 42).unwrap();
        assert!(!buf.is_empty());
        assert_eq!(buf.buffered_count(), 1);

        let _ = buf.next_in_order();
        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);
    }

    #[test]
    fn passthrough_finish_succeeds_when_drained() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        buf.insert(0, 0).unwrap();
        buf.insert(1, 1).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        buf.finish(); // no panic
    }

    #[test]
    #[should_panic(expected = "items remain undelivered")]
    fn passthrough_finish_panics_with_pending_items() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        buf.insert(0, 0).unwrap();
        buf.finish();
    }

    #[test]
    fn passthrough_take_returns_none() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        buf.insert(0, 42).unwrap();
        // Take is not supported in bypass mode.
        assert!(buf.take(0).is_none());
        // Item is still in the queue.
        assert_eq!(buf.buffered_count(), 1);
    }

    #[test]
    fn passthrough_capacity_is_zero() {
        let buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        assert_eq!(buf.capacity(), 0);
    }

    #[test]
    fn passthrough_stats_show_no_adaptive_events() {
        let buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        let stats = buf.stats();
        assert_eq!(stats.grow_events, 0);
        assert_eq!(stats.shrink_events, 0);
        assert_eq!(stats.capacity, 0);
    }

    #[test]
    fn passthrough_next_expected_advances() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();
        assert_eq!(buf.next_expected(), 0);

        buf.insert(5, 50).unwrap();
        let _ = buf.next_in_order();
        assert_eq!(buf.next_expected(), 1);

        buf.insert(3, 30).unwrap();
        buf.insert(1, 10).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        assert_eq!(buf.next_expected(), 3);
    }

    #[test]
    fn passthrough_large_batch() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();

        // Insert 1000 items with arbitrary sequence numbers.
        for i in 0..1000u64 {
            buf.insert(999 - i, i).unwrap();
        }
        assert_eq!(buf.buffered_count(), 1000);

        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 1000);
        // Values are in insertion order (0, 1, 2, ..., 999).
        for (i, &val) in drained.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
    }

    #[test]
    fn passthrough_interleaved_insert_and_drain() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::passthrough();

        buf.insert(0, 100).unwrap();
        assert_eq!(buf.next_in_order(), Some(100));

        buf.insert(5, 200).unwrap();
        buf.insert(3, 300).unwrap();
        assert_eq!(buf.next_in_order(), Some(200));
        assert_eq!(buf.next_in_order(), Some(300));
        assert!(buf.is_empty());
    }

    #[test]
    fn non_passthrough_flag_is_false() {
        let buf: ReorderBuffer<u64> = ReorderBuffer::new(4);
        assert!(!buf.is_passthrough());
    }

    /// Verifies that `DeltaResult` items flow through passthrough mode
    /// without sequence-based reordering.
    #[test]
    fn passthrough_delta_result_integration() {
        use crate::concurrent_delta::types::DeltaResult;

        let mut buf: ReorderBuffer<DeltaResult> = ReorderBuffer::passthrough();

        // Simulate three workers completing: seq 2 first, then 0, then 1.
        let r2 = DeltaResult::success(20, 2000, 500, 1500).with_sequence(2);
        let r0 = DeltaResult::success(10, 1000, 300, 700).with_sequence(0);
        let r1 = DeltaResult::needs_redo(15, "checksum mismatch".to_string()).with_sequence(1);

        buf.insert(r2.sequence(), r2).unwrap();
        buf.insert(r0.sequence(), r0).unwrap();
        buf.insert(r1.sequence(), r1).unwrap();

        let drained: Vec<DeltaResult> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 3);

        // Insertion order preserved - seq 2 first, then 0, then 1.
        assert_eq!(drained[0].sequence(), 2);
        assert_eq!(drained[0].ndx().get(), 20);

        assert_eq!(drained[1].sequence(), 0);
        assert_eq!(drained[1].ndx().get(), 10);

        assert_eq!(drained[2].sequence(), 1);
        assert_eq!(drained[2].ndx().get(), 15);
        assert!(drained[2].needs_retry());
    }
}

mod metrics_tests {
    use super::super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn metrics_start_zeroed() {
        let buf: ReorderBuffer<u32> = ReorderBuffer::new(8);
        let m = buf.metrics();
        assert_eq!(m.stall_duration, Duration::ZERO);
        assert_eq!(m.current_depth, 0);
        assert_eq!(m.max_depth, 0);
    }

    #[test]
    fn in_order_inserts_record_no_stall() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(8);
        buf.insert(0, 0).unwrap();
        buf.insert(1, 1).unwrap();
        buf.insert(2, 2).unwrap();
        let m = buf.metrics();
        assert_eq!(m.stall_duration, Duration::ZERO);
        assert_eq!(m.current_depth, 3);
        assert_eq!(m.max_depth, 3);
    }

    #[test]
    fn max_depth_tracks_high_water() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(16);
        for i in 0..5 {
            buf.insert(i, i as u32).unwrap();
        }
        assert_eq!(buf.metrics().max_depth, 5);
        // Drain three; depth falls but high-water stays.
        for _ in 0..3 {
            let _ = buf.next_in_order();
        }
        let m = buf.metrics();
        assert_eq!(m.current_depth, 2);
        assert_eq!(m.max_depth, 5);
    }

    #[test]
    fn out_of_order_insert_accumulates_stall_until_gap_closes() {
        let mut buf: ReorderBuffer<&'static str> = ReorderBuffer::new(8);
        // Seq 1 arrives first; next_expected is 0 so the buffer stalls.
        buf.insert(1, "second").unwrap();
        // Pre-close snapshot: stall has begun and is non-zero after a wait.
        sleep(Duration::from_millis(10));
        let mid = buf.metrics();
        assert!(
            mid.stall_duration >= Duration::from_millis(10),
            "expected stall >= 10ms while gap open, got {:?}",
            mid.stall_duration,
        );
        assert_eq!(mid.current_depth, 1);
        assert_eq!(mid.max_depth, 1);
        // Close the gap; stall accumulates to the closing event and stops.
        buf.insert(0, "first").unwrap();
        let after_close = buf.metrics();
        assert!(after_close.stall_duration >= mid.stall_duration);
        assert_eq!(after_close.current_depth, 2);
        assert_eq!(after_close.max_depth, 2);
        // Draining all items must not extend stall further.
        let _ = buf.next_in_order();
        let _ = buf.next_in_order();
        let drained = buf.metrics();
        // Allow microsecond jitter from the close event being recorded slightly
        // after the snapshot was taken.
        assert!(
            drained.stall_duration >= after_close.stall_duration,
            "stall must be monotonic",
        );
        // Once the buffer is empty, further sleeps don't grow the counter.
        let before_idle = drained.stall_duration;
        sleep(Duration::from_millis(10));
        let after_idle = buf.metrics().stall_duration;
        assert_eq!(
            after_idle, before_idle,
            "idle buffer must not accumulate stall time",
        );
    }

    #[test]
    fn multiple_stalls_accumulate() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(8);
        // First stall window.
        buf.insert(1, 1).unwrap();
        sleep(Duration::from_millis(5));
        buf.insert(0, 0).unwrap();
        let first = buf.metrics().stall_duration;
        assert!(first >= Duration::from_millis(5));
        // Drain everything; idle.
        while buf.next_in_order().is_some() {}
        // Second stall window with a fresh gap.
        buf.insert(3, 3).unwrap();
        sleep(Duration::from_millis(5));
        buf.insert(2, 2).unwrap();
        let second = buf.metrics().stall_duration;
        assert!(
            second >= first + Duration::from_millis(5),
            "second stall must add to the first: first={first:?} second={second:?}",
        );
    }

    #[test]
    fn stall_continues_across_take_when_gap_remains() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(8);
        // Two out-of-order items leave a gap at seq 0.
        buf.insert(1, 1).unwrap();
        buf.insert(2, 2).unwrap();
        sleep(Duration::from_millis(5));
        // Removing seq 2 via take() does not close the gap.
        let taken = buf.take(2);
        assert_eq!(taken, Some(2));
        sleep(Duration::from_millis(5));
        // Close the gap; total stall should cover both sleeps.
        buf.insert(0, 0).unwrap();
        let m = buf.metrics();
        assert!(
            m.stall_duration >= Duration::from_millis(10),
            "expected >=10ms stall across take(), got {:?}",
            m.stall_duration,
        );
    }

    #[test]
    fn metrics_snapshot_includes_in_flight_stall() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(8);
        buf.insert(1, 1).unwrap();
        sleep(Duration::from_millis(5));
        let snap_a = buf.metrics();
        sleep(Duration::from_millis(5));
        let snap_b = buf.metrics();
        assert!(
            snap_b.stall_duration > snap_a.stall_duration,
            "in-flight stall snapshots must increase: a={:?} b={:?}",
            snap_a.stall_duration,
            snap_b.stall_duration,
        );
    }

    #[test]
    fn force_insert_tracks_depth_and_stall() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(2);
        // force_insert past capacity grows the ring and registers a stall.
        buf.force_insert(3, 30);
        sleep(Duration::from_millis(5));
        let mid = buf.metrics();
        assert_eq!(mid.current_depth, 1);
        assert_eq!(mid.max_depth, 1);
        assert!(mid.stall_duration >= Duration::from_millis(5));
        // Closing the gap stops the stall counter from growing further.
        for seq in 0..3 {
            buf.force_insert(seq, seq as u32 * 10);
        }
        let closed = buf.metrics();
        assert_eq!(closed.max_depth, 4);
    }

    #[test]
    fn passthrough_tracks_depth_only() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::passthrough();
        buf.insert(5, 50).unwrap();
        buf.insert(3, 30).unwrap();
        sleep(Duration::from_millis(5));
        let m = buf.metrics();
        // Bypass mode has no sequence gap, so stalls never engage.
        assert_eq!(m.stall_duration, Duration::ZERO);
        assert_eq!(m.current_depth, 2);
        assert_eq!(m.max_depth, 2);
    }

    #[test]
    fn force_insert_count_starts_at_zero_and_increments() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(4);
        assert_eq!(buf.metrics().force_insert_count, 0);
        buf.force_insert(10, 100);
        buf.force_insert(11, 110);
        buf.force_insert(12, 120);
        assert_eq!(buf.metrics().force_insert_count, 3);
    }

    #[test]
    fn force_insert_count_increments_on_bypass_too() {
        // The bypass path still accumulates the counter so operators can
        // diagnose downstream queue pressure regardless of reordering mode.
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::passthrough();
        buf.force_insert(0, 0);
        buf.force_insert(0, 1);
        assert_eq!(buf.metrics().force_insert_count, 2);
    }

    #[test]
    fn force_insert_count_increments_when_sequence_behind_window() {
        // Sequences below next_expected are dropped by force_insert but the
        // call itself is still observed (it indicates upstream sent a stale
        // result that should never have hit the consumer).
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(4);
        buf.insert(0, 0).unwrap();
        let _ = buf.next_in_order();
        buf.force_insert(0, 99);
        assert_eq!(buf.metrics().force_insert_count, 1);
    }

    #[test]
    fn drain_batch_histogram_records_powers_of_two() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(16);
        buf.record_drain_batch(1);
        buf.record_drain_batch(2);
        buf.record_drain_batch(3);
        buf.record_drain_batch(8);
        buf.record_drain_batch(8);
        buf.record_drain_batch(2048);
        let hist = buf.metrics().drain_batch_size_histogram;
        let buckets = hist.buckets();
        assert_eq!(buckets[0], 1, "bucket 1 (size=1)");
        assert_eq!(buckets[1], 2, "bucket 2 (sizes 2,3)");
        assert_eq!(buckets[3], 2, "bucket 8 (size=8 x2)");
        assert_eq!(buckets[10], 1, "bucket >=1024 (size=2048)");
        assert_eq!(hist.total_samples(), 6);
    }

    #[test]
    fn drain_pause_histogram_records_microsecond_decades() {
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(4);
        buf.record_drain_pause(Duration::from_nanos(100)); // <1us
        buf.record_drain_pause(Duration::from_micros(5)); // 1-10us
        buf.record_drain_pause(Duration::from_micros(50)); // 10-100us
        buf.record_drain_pause(Duration::from_micros(500)); // 100-1000us
        buf.record_drain_pause(Duration::from_millis(5)); // 1000-10000us
        buf.record_drain_pause(Duration::from_millis(50)); // >=10000us
        buf.record_drain_pause(Duration::from_secs(1)); // >=10000us
        let hist = buf.metrics().drain_pause_histogram;
        let buckets = hist.buckets();
        assert_eq!(buckets, [1, 1, 1, 1, 1, 2]);
        assert_eq!(hist.total_samples(), 7);
    }

    #[test]
    fn drain_batch_zero_does_not_record() {
        // Empty drains must not pollute the distribution.
        let mut buf: ReorderBuffer<u32> = ReorderBuffer::new(4);
        buf.record_drain_batch(0);
        buf.record_drain_batch(0);
        assert_eq!(buf.metrics().drain_batch_size_histogram.total_samples(), 0);
    }

    #[test]
    fn in_flight_window_tracks_furthest_ahead_arrival() {
        let mut buf = ReorderBuffer::new(8);
        assert_eq!(buf.in_flight_window(), 0, "empty buffer has no window");

        // A single in-order arrival spans one slot.
        buf.insert(0, "a").unwrap();
        assert_eq!(buf.in_flight_window(), 1);

        // A far-ahead out-of-order arrival stretches the window to its
        // offset+1 even though only two slots are occupied - distinguishing
        // the window from buffered_count (the spill-pressure signal ROB-2
        // gates on).
        buf.insert(3, "d").unwrap();
        assert_eq!(buf.in_flight_window(), 4);
        assert_eq!(buf.buffered_count(), 2);

        // Filling an interior gap does not shrink the window.
        buf.insert(1, "b").unwrap();
        assert_eq!(buf.in_flight_window(), 4);
    }

    #[test]
    fn in_flight_window_shrinks_as_items_are_yielded() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        assert_eq!(buf.in_flight_window(), 2);

        // Draining the contiguous prefix advances the delivery cursor and
        // collapses the window back toward zero.
        let drained: Vec<_> = buf.drain_ready().collect();
        assert_eq!(drained, vec!["a", "b"]);
        assert_eq!(buf.in_flight_window(), 0);
    }
}
