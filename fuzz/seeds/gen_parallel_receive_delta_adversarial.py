#!/usr/bin/env python3
"""Generate the seed corpus for the parallel_receive_delta_adversarial target.

The seeds must be produced against the target's own `parse_input`, never
hand-written: a byte string that does not parse yields either zero records or a
single chunk, and the smoke run (`cargo fuzz run <target> -- -runs=0`, which
executes the corpus and nothing else) then exercises no ordering at all while
still reporting success.

Input format (fuzz_targets/parallel_receive_delta_adversarial.rs):

    u8  file_count        -> (byte % 8) + 1
    u8  payload_size_log2 -> 4 + (byte % 7), payload bytes = 1 << that
    records: [ u8 file_ndx (% file_count), u16le chunk_sequence, payload ] *

Chunk sequences are renumbered per file into a dense 0..N range in ascending
value order, so emitting ascending values keeps a file's arrival order and
emitting descending values reverses it.

Usage: python3 fuzz/seeds/gen_parallel_receive_delta_adversarial.py
"""

import pathlib

CORPUS = pathlib.Path(__file__).resolve().parents[1] / "corpus" / "parallel_receive_delta_adversarial"


def payload(file_ndx: int, sequence: int, size: int) -> bytes:
    """Return a payload unique to (file_ndx, sequence).

    A chunk written into the wrong file or at the wrong offset changes that
    file's SHA-256 only if no two chunks share a payload.
    """
    return bytes((file_ndx * 37 + sequence * 11 + i) % 256 for i in range(size))


def encode(file_count: int, payload_log2: int, arrivals: list[tuple[int, int]]) -> bytes:
    size = 1 << payload_log2
    out = bytearray([file_count - 1, payload_log2 - 4])
    for file_ndx, sequence in arrivals:
        out.append(file_ndx)
        out += sequence.to_bytes(2, "little")
        out += payload(file_ndx, sequence, size)
    return bytes(out)


def threshold_trip() -> bytes:
    """Eight files, four chunks each, first-registered file written last.

    Files 0-3 arrive in order and files 4-7 fully reversed, so one stream
    exercises both the straight-through drain and the reorder-buffer drain
    while the file count crosses the dispatch threshold mid-stream.
    """
    arrivals = []
    for round_ndx in range(4):
        for file_ndx in [1, 2, 3, 4, 5, 6, 7, 0]:
            sequence = round_ndx if file_ndx < 4 else 3 - round_ndx
            arrivals.append((file_ndx, sequence))
    return encode(8, 4, arrivals)


def slot_recycle_race() -> bytes:
    """One file completes and frees its slot before the others claim slots.

    File 0 is delivered whole and in order, so its slot is released while the
    later files are still being registered; file 3 then arrives fully reversed,
    holding every chunk in the reorder buffer until sequence 0 lands.
    """
    arrivals = [(0, sequence) for sequence in range(6)]
    for sequence in range(4):
        arrivals.append((1, sequence))
        arrivals.append((2, sequence))
    arrivals += [(3, sequence) for sequence in reversed(range(8))]
    return encode(4, 5, arrivals)


def main() -> None:
    for name, data in (
        ("pip_7_threshold_trip.bin", threshold_trip()),
        ("dg_3_slot_recycle_race.bin", slot_recycle_race()),
    ):
        (CORPUS / name).write_bytes(data)
        print(f"{name}: {len(data)} bytes")


if __name__ == "__main__":
    main()
