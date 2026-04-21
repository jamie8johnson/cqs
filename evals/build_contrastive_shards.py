#!/usr/bin/env python3
"""Pre-compute contrastive ranking training shards for the fused alpha + classifier head.

For each query in the v3 + synthetic corpus:
  1. Run cqs batch search to get top-50 candidates (default routing).
  2. For each candidate, compute SPLADE sparse score and BGE dense cosine
     against the query — independent of cqs's RRF blend (we want the raw
     score components, not the production combined score).
  3. Identify the gold candidate by (file, name, line_start) match against
     the query's gold_chunk.
  4. Sample 15 distractors stratified by combined score quantile (5 per
     quartile of top/middle/bottom thirds).
  5. Compute the corpus fingerprint (mean of all chunk embeddings, L2-norm).
  6. Save one .npz with everything the trainer needs.

The output is consumed by `evals/train_fused_head.py`. The shard layout
is documented in the spec at
`docs/plans/2026-04-20-fused-alpha-classifier-head.md`.

Run:
    python3 evals/build_contrastive_shards.py \\
        --output evals/fused_head/contrastive_shards.npz
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path
from typing import Optional

import numpy as np

REPO = Path(__file__).resolve().parent.parent
QUERIES_DIR = REPO / "evals" / "queries"
SPLADE_DIR = Path.home() / ".cache/huggingface/splade-onnx"

# Match the order in evals/train_query_classifier.py and src/classifier_head.rs.
CATEGORIES = [
    "identifier_lookup",
    "structural_search",
    "behavioral_search",
    "conceptual_search",
    "multi_step",
    "negation",
    "type_filtered",
    "cross_language",
]
CAT_TO_IDX = {c: i for i, c in enumerate(CATEGORIES)}
CAT_ALIASES = {
    "behavioral": "behavioral_search",
    "structural": "structural_search",
    "conceptual": "conceptual_search",
}


def gold_key(g: dict) -> tuple:
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def candidate_key(c: dict) -> tuple:
    return (c.get("file"), c.get("name"), c.get("line_start"))


def load_corpus() -> list[dict]:
    """Combined v3 (train+test+dev) + synthetic queries."""
    out = []
    for split, path in [
        ("train", QUERIES_DIR / "v3_train.json"),
        ("test", QUERIES_DIR / "v3_test.v2.json"),
        ("dev", QUERIES_DIR / "v3_dev.v2.json"),
    ]:
        if not path.exists():
            continue
        rows = json.loads(path.read_text())["queries"]
        for r in rows:
            cat = r.get("category")
            if not cat:
                continue
            cat = CAT_ALIASES.get(cat, cat)
            if cat not in CAT_TO_IDX:
                continue
            out.append({**r, "category": cat, "split": split})

    synth_path = QUERIES_DIR / "v3_generated_round1.json"
    if synth_path.exists():
        synth = json.loads(synth_path.read_text())["queries"]
        for r in synth:
            if not r.get("matched", False):
                continue
            cat = r.get("category")
            if cat:
                cat = CAT_ALIASES.get(cat, cat)
            if cat not in CAT_TO_IDX:
                continue
            out.append({**r, "category": cat, "split": "synthetic"})
    return out


def cqs_top_k(queries: list[str], k: int = 50) -> list[list[dict]]:
    """Run cqs batch search through the daemon. Returns top-k per query."""
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/build-shards.stderr", "ab"),
        text=True, bufsize=1,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd = f"search {shlex.quote(q)} --limit {k} --splade"
            try:
                proc.stdin.write(cmd + "\n")
                proc.stdin.flush()
            except (BrokenPipeError, OSError):
                break
            line = proc.stdout.readline()
            if not line:
                break
            try:
                envelope = json.loads(line)
                payload = envelope.get("data") if isinstance(envelope.get("data"), dict) else envelope
                out.append(payload.get("results", []) if payload else [])
            except json.JSONDecodeError:
                out.append([])
            if (i + 1) % 100 == 0 or i + 1 == len(queries):
                rate = (i + 1) / max(time.monotonic() - t0, 0.01)
                print(f"  [search] {i+1}/{len(queries)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def load_bge():
    from sentence_transformers import SentenceTransformer
    print("[load] BGE-large-en-v1.5", file=sys.stderr)
    m = SentenceTransformer("BAAI/bge-large-en-v1.5")
    return m


def load_splade():
    """Load SPLADE ONNX session + tokenizer using the cqs-deployed model."""
    if not SPLADE_DIR.exists():
        raise SystemExit(f"SPLADE model dir not found: {SPLADE_DIR}")
    import onnxruntime as ort
    from tokenizers import Tokenizer

    print(f"[load] SPLADE from {SPLADE_DIR}", file=sys.stderr)
    sess_opts = ort.SessionOptions()
    sess_opts.intra_op_num_threads = 4
    providers = ["CUDAExecutionProvider", "CPUExecutionProvider"]
    sess = ort.InferenceSession(str(SPLADE_DIR / "model.onnx"),
                                sess_options=sess_opts, providers=providers)
    tok = Tokenizer.from_file(str(SPLADE_DIR / "tokenizer.json"))
    return sess, tok


def splade_vectors(sess, tok, texts: list[str], batch_size: int = 16,
                   max_seq_len: int = 256) -> np.ndarray:
    """Compute SPLADE sparse vectors for a list of texts.

    Mirrors `src/splade/mod.rs::encode`:
      tokenize → ONNX forward → max-pool over seq → ReLU + log1p.

    Returns dense [N, vocab] numpy array (sparse vectors stored densely
    for training-shard simplicity; the matmul for scoring is fast enough).
    """
    # Detect output name (sparse_vector vs logits) on first invocation.
    out_names = [o.name for o in sess.get_outputs()]
    has_sparse = "sparse_vector" in out_names
    out_name = "sparse_vector" if has_sparse else "logits"

    all_vecs = []
    for i in range(0, len(texts), batch_size):
        batch = texts[i:i + batch_size]
        # Tokenize with truncation + padding.
        encoded = tok.encode_batch(batch)
        ids_list = [e.ids[:max_seq_len] for e in encoded]
        max_len = max(len(ids) for ids in ids_list)
        input_ids = np.zeros((len(batch), max_len), dtype=np.int64)
        attn_mask = np.zeros((len(batch), max_len), dtype=np.int64)
        for j, ids in enumerate(ids_list):
            input_ids[j, :len(ids)] = ids
            attn_mask[j, :len(ids)] = 1
        feed = {"input_ids": input_ids, "attention_mask": attn_mask}
        # Some SPLADE models also expect token_type_ids.
        in_names = {x.name for x in sess.get_inputs()}
        if "token_type_ids" in in_names:
            feed["token_type_ids"] = np.zeros_like(input_ids)
        outs = sess.run([out_name], feed)[0]  # [batch, seq, vocab] or [batch, vocab]
        if outs.ndim == 3:
            # Mask padding before max-pool.
            mask = attn_mask[:, :, None].astype(np.float32)
            outs = np.where(mask > 0, outs, -np.inf)
            pooled = outs.max(axis=1)  # [batch, vocab]
            activated = np.log1p(np.maximum(0, pooled))
        else:
            # Pre-pooled (sparse_vector output).
            activated = outs
        all_vecs.append(activated.astype(np.float32))
    return np.concatenate(all_vecs, axis=0)


def stratified_distractors(scores: np.ndarray, gold_idx: int, k: int = 15) -> list[int]:
    """Pick k distractors stratified by combined-score quantile.

    Splits the non-gold candidates into thirds by score, samples k//3 from
    each. Falls back to random sampling if the pool is too small.
    """
    n = len(scores)
    indices = [i for i in range(n) if i != gold_idx]
    if len(indices) <= k:
        return indices
    # Sort by score descending.
    indices.sort(key=lambda i: scores[i], reverse=True)
    third = max(1, len(indices) // 3)
    top = indices[:third]
    mid = indices[third:2 * third]
    bot = indices[2 * third:]
    per_bin = k // 3
    extras = k - per_bin * 3
    rng = np.random.default_rng(seed=42 + gold_idx)
    picks = (
        list(rng.choice(top, size=min(per_bin + (1 if extras > 0 else 0), len(top)), replace=False))
        + list(rng.choice(mid, size=min(per_bin + (1 if extras > 1 else 0), len(mid)), replace=False))
        + list(rng.choice(bot, size=min(per_bin, len(bot)), replace=False))
    )
    # Pad with random if we're under k due to small bins.
    if len(picks) < k:
        remaining = [i for i in indices if i not in picks]
        more = list(rng.choice(remaining, size=k - len(picks), replace=False))
        picks.extend(more)
    return picks[:k]


def compute_corpus_fingerprint(store_db: Path = Path("/mnt/c/Projects/cqs/.cqs/index.db"),
                               dim: int = 1024) -> Optional[np.ndarray]:
    """Compute the corpus fingerprint = normalize(mean(chunk_embeddings)).

    Tries the cqs-managed cache first; falls back to direct SQLite scan.
    Writes the cache after a successful Python compute so the Rust loader
    picks it up next.
    """
    cache = Path.home() / ".local/share/cqs/corpus_fingerprint.v1.bin"
    if cache.exists():
        bytes_data = cache.read_bytes()
        if len(bytes_data) == dim * 4:
            print(f"[fingerprint] hit cache at {cache}", file=sys.stderr)
            return np.frombuffer(bytes_data, dtype=np.float32).copy()
        print(f"[fingerprint] cache dim mismatch ({len(bytes_data)} bytes); recomputing",
              file=sys.stderr)

    if not store_db.exists():
        print(f"[fingerprint] store DB not found at {store_db}", file=sys.stderr)
        return None

    print(f"[fingerprint] scanning {store_db} (this may take ~30s on a "
          "15k-chunk corpus)", file=sys.stderr)
    import sqlite3
    conn = sqlite3.connect(str(store_db))
    cur = conn.cursor()
    cur.execute("SELECT embedding FROM chunks")
    sum_vec = np.zeros(dim, dtype=np.float64)
    count = 0
    for (blob,) in cur:
        if blob is None or len(blob) != dim * 4:
            continue
        v = np.frombuffer(blob, dtype=np.float32)
        sum_vec += v
        count += 1
    conn.close()
    if count == 0:
        print("[fingerprint] zero embeddings in store", file=sys.stderr)
        return None
    mean = (sum_vec / count).astype(np.float32)
    norm = np.linalg.norm(mean)
    if norm <= 0:
        print("[fingerprint] mean is zero — degenerate corpus", file=sys.stderr)
        return None
    fingerprint = (mean / norm).astype(np.float32)
    print(f"[fingerprint] computed from {count} chunks; ||v||={norm:.4f}",
          file=sys.stderr)

    # Cache for the Rust loader (and for re-runs of this script).
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_bytes(fingerprint.tobytes())
    print(f"[fingerprint] cached to {cache}", file=sys.stderr)
    return fingerprint


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--top-k", type=int, default=50,
                    help="Candidates retrieved per query before sampling")
    ap.add_argument("--n-distractors", type=int, default=15,
                    help="Distractors per query (gold + n distractors = pool size)")
    ap.add_argument("--max-queries", type=int, default=None,
                    help="Smoke-test cap")
    ap.add_argument("--no-resume", action="store_true",
                    help="Ignore checkpoint files and recompute from scratch")
    args = ap.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Resumability: cache the expensive intermediate artifacts so a crash
    # in step 3 doesn't force re-running search (~7 min) and SPLADE (~16 min).
    search_ckpt = args.output.with_suffix(".search.pkl")
    encoded_ckpt = args.output.with_suffix(".encoded.npz")

    rows = load_corpus()
    if args.max_queries:
        rows = rows[:args.max_queries]
    print(f"[load] {len(rows)} queries (v3 + synthetic)", file=sys.stderr)
    cat_counts = defaultdict(int)
    split_counts = defaultdict(int)
    for r in rows:
        cat_counts[r["category"]] += 1
        split_counts[r["split"]] += 1
    print(f"[load] per-category: {dict(cat_counts)}", file=sys.stderr)
    print(f"[load] per-split: {dict(split_counts)}", file=sys.stderr)

    queries = [r["query"] for r in rows]

    # 1. Top-k candidates per query (resumable via search checkpoint).
    if search_ckpt.exists() and not args.no_resume:
        import pickle
        with open(search_ckpt, "rb") as f:
            ckpt = pickle.load(f)
        if ckpt.get("n_queries") == len(queries) and ckpt.get("top_k") == args.top_k:
            print(f"\n[step 1] reusing search checkpoint {search_ckpt} "
                  f"({ckpt['n_queries']} queries, top-{ckpt['top_k']})",
                  file=sys.stderr)
            candidates_per_query = ckpt["candidates_per_query"]
        else:
            print(f"\n[step 1] checkpoint mismatch (n={ckpt.get('n_queries')} "
                  f"vs {len(queries)}, k={ckpt.get('top_k')} vs {args.top_k}); "
                  "recomputing search", file=sys.stderr)
            candidates_per_query = None
    else:
        candidates_per_query = None

    if candidates_per_query is None:
        print(f"\n[step 1] cqs batch search top-{args.top_k}", file=sys.stderr)
        candidates_per_query = cqs_top_k(queries, k=args.top_k)
        import pickle
        with open(search_ckpt, "wb") as f:
            pickle.dump({"n_queries": len(queries), "top_k": args.top_k,
                         "candidates_per_query": candidates_per_query}, f)
        print(f"  saved search checkpoint to {search_ckpt}", file=sys.stderr)
    n_cand = sum(len(c) for c in candidates_per_query)
    print(f"  retrieved {n_cand} candidate-results "
          f"(avg {n_cand / max(len(rows), 1):.1f} per query)", file=sys.stderr)

    # Build dedup mapping (cheap; redo each run).
    content_to_idx = {}
    query_cand_indices = []
    for cands in candidates_per_query:
        idxs = []
        for c in cands:
            content = c.get("content", "") or ""
            if content not in content_to_idx:
                content_to_idx[content] = len(content_to_idx)
            idxs.append(content_to_idx[content])
        query_cand_indices.append(idxs)
    unique_contents = [None] * len(content_to_idx)
    for content, idx in content_to_idx.items():
        unique_contents[idx] = content
    print(f"  dedup → {len(unique_contents)} unique contents "
          f"({100*len(unique_contents)/max(n_cand, 1):.1f}% unique)",
          file=sys.stderr)

    # 2. Score components (resumable via encoded checkpoint).
    if encoded_ckpt.exists() and not args.no_resume:
        ckpt = np.load(encoded_ckpt, allow_pickle=True)
        if (ckpt["n_queries"].item() == len(queries) and
                ckpt["n_unique"].item() == len(unique_contents)):
            print(f"\n[step 2] reusing encoded checkpoint {encoded_ckpt} "
                  f"(queries={len(queries)}, unique={len(unique_contents)})",
                  file=sys.stderr)
            query_embs = ckpt["query_embs"]
            query_splade = ckpt["query_splade"]
            cand_bge = ckpt["cand_bge"]
            cand_splade = ckpt["cand_splade"]
        else:
            print(f"\n[step 2] encoded checkpoint mismatch "
                  f"(queries: {ckpt['n_queries'].item()} vs {len(queries)}, "
                  f"unique: {ckpt['n_unique'].item()} vs {len(unique_contents)}); "
                  "re-encoding", file=sys.stderr)
            query_embs = None
    else:
        query_embs = None

    if query_embs is None:
        print("\n[step 2] BGE + SPLADE score decomposition", file=sys.stderr)
        bge = load_bge()
        splade_sess, splade_tok = load_splade()

        print("  [bge] embedding queries", file=sys.stderr)
        query_embs = bge.encode(queries, batch_size=64, show_progress_bar=True,
                                normalize_embeddings=True).astype(np.float32)

        print("  [splade] encoding queries", file=sys.stderr)
        query_splade = splade_vectors(splade_sess, splade_tok, queries, batch_size=16)

        print("  [bge] encoding unique candidates", file=sys.stderr)
        t0 = time.monotonic()
        cand_bge = bge.encode(unique_contents, batch_size=64, show_progress_bar=True,
                               normalize_embeddings=True).astype(np.float32)
        print(f"  [bge] done in {time.monotonic()-t0:.1f}s", file=sys.stderr)

        print("  [splade] encoding unique candidates", file=sys.stderr)
        t0 = time.monotonic()
        cand_splade = splade_vectors(splade_sess, splade_tok, unique_contents,
                                      batch_size=32)
        print(f"  [splade] done in {time.monotonic()-t0:.1f}s "
              f"(shape {cand_splade.shape})", file=sys.stderr)

        np.savez_compressed(
            encoded_ckpt,
            n_queries=np.array(len(queries)),
            n_unique=np.array(len(unique_contents)),
            query_embs=query_embs,
            query_splade=query_splade,
            cand_bge=cand_bge,
            cand_splade=cand_splade,
        )
        print(f"  saved encoded checkpoint to {encoded_ckpt}", file=sys.stderr)

    # 3. Per-query scoring via dot products on pre-computed vectors (pure numpy, fast).
    print("\n[step 3] per-query scoring (pure numpy)", file=sys.stderr)
    pool_size = 1 + args.n_distractors
    Q = len(rows)
    out_query_emb = np.zeros((Q, 1024), dtype=np.float32)
    out_sparse = np.zeros((Q, pool_size), dtype=np.float32)
    out_dense = np.zeros((Q, pool_size), dtype=np.float32)
    out_gold_idx = np.full((Q,), -1, dtype=np.int64)
    out_cat = np.full((Q,), -1, dtype=np.int64)
    out_valid = np.zeros((Q,), dtype=bool)

    t0 = time.monotonic()
    for qi, (row, cands) in enumerate(zip(rows, candidates_per_query)):
        if not cands:
            continue
        gold = row.get("gold_chunk") or {}
        gk = gold_key(gold)
        gold_local_idx = None
        for ci, c in enumerate(cands):
            if candidate_key(c) == gk:
                gold_local_idx = ci
                break
        if gold_local_idx is None:
            continue

        # Lookup pre-computed vectors for this query's candidates.
        cand_uniq_idxs = query_cand_indices[qi]
        cand_b = cand_bge[cand_uniq_idxs]        # [n_cand, 1024]
        cand_s = cand_splade[cand_uniq_idxs]     # [n_cand, vocab]
        dense_scores = cand_b @ query_embs[qi]
        sparse_scores = cand_s @ query_splade[qi]

        combined = 0.5 * sparse_scores + 0.5 * dense_scores
        distractor_idxs = stratified_distractors(combined, gold_local_idx,
                                                  k=args.n_distractors)
        # Skip queries that don't yield a full pool (too few candidates).
        if len(distractor_idxs) < args.n_distractors:
            continue
        pool_idxs = [gold_local_idx] + distractor_idxs

        out_query_emb[qi] = query_embs[qi]
        out_sparse[qi] = sparse_scores[pool_idxs]
        out_dense[qi] = dense_scores[pool_idxs]
        out_gold_idx[qi] = 0
        out_cat[qi] = CAT_TO_IDX[row["category"]]
        out_valid[qi] = True

        if (qi + 1) % 500 == 0 or qi + 1 == Q:
            rate = (qi + 1) / max(time.monotonic() - t0, 0.01)
            valid_so_far = int(out_valid[:qi + 1].sum())
            print(f"  {qi+1}/{Q} ({rate:.0f} q/s) — "
                  f"valid {valid_so_far} (gold-in-top-{args.top_k})",
                  file=sys.stderr, flush=True)

    # 4. Corpus fingerprint.
    print("\n[step 4] corpus fingerprint", file=sys.stderr)
    fingerprint = compute_corpus_fingerprint()
    if fingerprint is None:
        print("  WARN: no fingerprint available — using zeros (training will "
              "operate with the fingerprint channel constant at 0)",
              file=sys.stderr)
        fingerprint = np.zeros((1024,), dtype=np.float32)
    else:
        print(f"  loaded {fingerprint.shape[0]}-dim fingerprint", file=sys.stderr)

    # 5. Filter to valid rows + write.
    valid = out_valid
    n_valid = int(valid.sum())
    print(f"\n[save] {n_valid}/{Q} queries with gold in top-{args.top_k} "
          f"({100*n_valid/max(Q,1):.1f}%)", file=sys.stderr)

    np.savez_compressed(
        args.output,
        query_emb=out_query_emb[valid],
        sparse_scores=out_sparse[valid],
        dense_scores=out_dense[valid],
        gold_idx=out_gold_idx[valid],
        category=out_cat[valid],
        fingerprint=fingerprint,
        category_names=np.array(CATEGORIES, dtype=object),
    )
    print(f"[save] wrote {args.output} ({args.output.stat().st_size / 1024:.1f} KB)",
          file=sys.stderr)


if __name__ == "__main__":
    main()
