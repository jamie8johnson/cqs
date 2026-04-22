#!/usr/bin/env python3
"""
Project chunk embeddings into 2D via UMAP for the cqs serve cluster view.

Stdin protocol (binary, length-prefixed):
    Header (12 bytes): u32 little-endian n_rows, u32 little-endian dim, u32 little-endian id_max_len
    Repeated n_rows times:
        u16 little-endian id_len
        id_len bytes (utf-8 chunk_id)
        dim * 4 bytes (f32 little-endian embedding)

Stdout protocol (utf-8 text):
    n_rows lines of "<chunk_id>\t<x>\t<y>"
    `x` and `y` are decimal floats with 6-digit precision.

Stderr: progress messages + UMAP fit/transform diagnostics.

Exit codes:
    0  — success
    1  — bad input shape / parse failure
    2  — UMAP fit/transform failure (rare; usually OOM or a degenerate dataset)
    3  — umap-learn missing (umap-learn must be importable as `umap`)

Invoked by `cqs index --umap`. Run directly:
    python3 scripts/run_umap.py < embeddings.bin > coords.tsv
"""

from __future__ import annotations

import struct
import sys
import time
from typing import List, Tuple

try:
    import numpy as np
except ImportError as e:
    print(f"run_umap: numpy import failed: {e}", file=sys.stderr)
    sys.exit(3)

try:
    import umap  # umap-learn package
except ImportError as e:
    print(
        f"run_umap: umap-learn import failed: {e}\n"
        "  install with: pip install umap-learn",
        file=sys.stderr,
    )
    sys.exit(3)


def read_input() -> Tuple[List[str], np.ndarray]:
    raw = sys.stdin.buffer.read()
    if len(raw) < 12:
        raise ValueError(f"input too short: got {len(raw)} bytes, need ≥12 for header")

    n_rows, dim, _id_max_len = struct.unpack_from("<III", raw, 0)
    print(f"run_umap: header n_rows={n_rows} dim={dim}", file=sys.stderr)

    if n_rows == 0:
        return [], np.zeros((0, dim), dtype=np.float32)

    ids: List[str] = []
    vectors = np.zeros((n_rows, dim), dtype=np.float32)
    pos = 12
    for i in range(n_rows):
        if pos + 2 > len(raw):
            raise ValueError(f"row {i}: truncated id-length prefix")
        (id_len,) = struct.unpack_from("<H", raw, pos)
        pos += 2

        if pos + id_len > len(raw):
            raise ValueError(f"row {i}: truncated id ({id_len} bytes claimed)")
        ids.append(raw[pos : pos + id_len].decode("utf-8"))
        pos += id_len

        nbytes = dim * 4
        if pos + nbytes > len(raw):
            raise ValueError(f"row {i}: truncated embedding ({nbytes} bytes claimed)")
        vec = np.frombuffer(raw[pos : pos + nbytes], dtype="<f4")
        vectors[i] = vec
        pos += nbytes

    if pos != len(raw):
        # Not fatal; could be padding. Warn but proceed.
        print(
            f"run_umap: warning — {len(raw) - pos} trailing bytes after row {n_rows}",
            file=sys.stderr,
        )
    return ids, vectors


def project(vectors: np.ndarray) -> np.ndarray:
    n_rows, dim = vectors.shape
    # Sane defaults that work well on BGE-large code embeddings (1024-dim,
    # ~16k chunks). n_neighbors=15 + min_dist=0.1 are the umap-learn defaults
    # but we set them explicitly so the projection is reproducible across
    # umap-learn versions. random_state=42 makes the layout deterministic.
    n_neighbors = min(15, max(2, n_rows - 1))
    print(
        f"run_umap: fit_transform n_rows={n_rows} dim={dim} n_neighbors={n_neighbors}",
        file=sys.stderr,
    )
    t0 = time.time()
    reducer = umap.UMAP(
        n_components=2,
        n_neighbors=n_neighbors,
        min_dist=0.1,
        metric="cosine",
        random_state=42,
        verbose=False,
    )
    coords = reducer.fit_transform(vectors)
    elapsed = time.time() - t0
    print(
        f"run_umap: projection done in {elapsed:.1f}s — "
        f"x ∈ [{coords[:,0].min():.2f}, {coords[:,0].max():.2f}], "
        f"y ∈ [{coords[:,1].min():.2f}, {coords[:,1].max():.2f}]",
        file=sys.stderr,
    )
    return coords


def main() -> int:
    try:
        ids, vectors = read_input()
    except (ValueError, struct.error) as e:
        print(f"run_umap: parse error: {e}", file=sys.stderr)
        return 1

    if vectors.shape[0] < 2:
        # UMAP needs at least 2 points to project. Echo the lone point at
        # the origin so the caller can still UPDATE without special-casing
        # the empty case.
        out = sys.stdout
        for chunk_id in ids:
            out.write(f"{chunk_id}\t0.000000\t0.000000\n")
        print(
            f"run_umap: only {vectors.shape[0]} rows — projection skipped, wrote origin coords",
            file=sys.stderr,
        )
        return 0

    try:
        coords = project(vectors)
    except (ValueError, MemoryError, RuntimeError) as e:
        print(f"run_umap: UMAP failed: {type(e).__name__}: {e}", file=sys.stderr)
        return 2

    out = sys.stdout
    for chunk_id, (x, y) in zip(ids, coords):
        out.write(f"{chunk_id}\t{x:.6f}\t{y:.6f}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
