#!/usr/bin/env python3
"""Utility helpers for computing SHA-256 digests.

This module provides a small CLI used by packaging scripts to compute
cryptographic checksums without depending on platform-specific shell utilities
such as ``sha256sum``. The :func:`compute_sha256` helper is also imported by the
unit tests under ``tools/tests`` so the implementation remains well exercised.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path
from typing import Iterable

# Default chunk size used while reading files from disk. The value strikes a
# balance between minimising system calls and keeping memory usage modest even
# when hashing large release archives.
_DEFAULT_CHUNK_SIZE = 1 << 20  # 1 MiB


def compute_sha256(path: Path, *, chunk_size: int = _DEFAULT_CHUNK_SIZE) -> str:
    """Return the hexadecimal SHA-256 digest for ``path``.

    Parameters
    ----------
    path:
        The filesystem location of the file to hash.
    chunk_size:
        The number of bytes read per iteration while streaming the file.

    Returns
    -------
    str
        The lowercase hexadecimal SHA-256 digest.

    Raises
    ------
    FileNotFoundError
        If ``path`` does not exist or is not a regular file.
    ValueError
        If ``chunk_size`` is not a positive integer.
    OSError
        Propagated for underlying I/O errors encountered while reading the
        file.
    """

    if chunk_size <= 0:
        raise ValueError("chunk_size must be a positive integer")

    if not path.is_file():
        raise FileNotFoundError(path)

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            data = handle.read(chunk_size)
            if not data:
                break
            digest.update(data)
    return digest.hexdigest()


def _positive_int(value: str) -> int:
    number = int(value, 10)
    if number <= 0:
        raise argparse.ArgumentTypeError("chunk size must be a positive integer")
    return number


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    """Parse CLI arguments for the checksum helper."""

    parser = argparse.ArgumentParser(
        description=(
            "Compute the SHA-256 digest for a file without relying on platform-"
            "specific utilities."
        )
    )
    parser.add_argument("path", type=Path, help="Path to the file to hash")
    parser.add_argument(
        "--chunk-size",
        type=_positive_int,
        default=_DEFAULT_CHUNK_SIZE,
        metavar="BYTES",
        help="Read BYTES per iteration while hashing (default: %(default)s)",
    )
    return parser.parse_args(list(argv) if argv is not None else None)


def main(argv: Iterable[str] | None = None) -> int:
    """Entry point used by ``__main__`` and shell scripts."""

    try:
        options = parse_args(argv)
        digest = compute_sha256(options.path, chunk_size=options.chunk_size)
    except FileNotFoundError as error:
        print(f"error: file not found: {error}", file=sys.stderr)
        return 1
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"error: failed to read {error.filename}: {error.strerror}", file=sys.stderr)
        return 1

    print(digest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
