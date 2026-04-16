#!/usr/bin/env python3
"""Centroid-based query classifier experiment on the v3 eval dataset.

Embeds queries with BGE-large (same model cqs uses for retrieval),
computes per-category mean centroids, evaluates accuracy via:
  1. Leave-one-out cross-validation on train (honest, no leakage)
  2. Dev-set accuracy (centroid-only, centroid+rule fallback)
  3. Confidence-threshold sweep on dev (margin between top-1 and top-2)

Saves centroids to ~/.local/share/cqs/classifier_centroids.v1.json
for later integration into src/search/classifier/centroid.rs.

Usage:
    conda activate cqs-train  # needs sentence-transformers + torch
    python3 evals/centroid_classifier.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np

QUERIES_DIR = Path(__file__).parent / "queries"
TRAIN_PATH = QUERIES_DIR / "v3_train.json"
DEV_PATH = QUERIES_DIR / "v3_dev.json"
TEST_PATH = QUERIES_DIR / "v3_test.json"

CENTROIDS_OUT = Path("~/.local/share/cqs/classifier_centroids.v1.json").expanduser()

MODEL_NAME = "BAAI/bge-large-en-v1.5"
# BGE-large uses "Represent this sentence: " prefix for queries per the model card.
# cqs uses the same prefix. Must match for centroid→runtime consistency.
BGE_QUERY_PREFIX = "Represent this sentence: "

CATEGORIES = [
    "identifier_lookup",
    "behavioral_search",
    "conceptual_search",
    "type_filtered",
    "cross_language",
    "structural_search",
    "negation",
    "multi_step",
]


def load_split(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    return [q for q in data["queries"] if q.get("category") in CATEGORIES]


def embed_queries(queries: list[str], model) -> np.ndarray:
    """Embed with BGE-large. Returns (N, 1024) float32 normalized."""
    # sentence-transformers handles the prefix internally for BGE if
    # prompt_name="query" is set, but let's be explicit to match cqs.
    prefixed = [BGE_QUERY_PREFIX + q for q in queries]
    embs = model.encode(prefixed, normalize_embeddings=True, show_progress_bar=True)
    return np.array(embs, dtype=np.float32)


def compute_centroids(embeddings: np.ndarray, labels: list[str]) -> dict[str, np.ndarray]:
    """Mean-pool embeddings per category, L2-normalize."""
    by_cat: dict[str, list[int]] = defaultdict(list)
    for i, lab in enumerate(labels):
        by_cat[lab].append(i)
    centroids = {}
    for cat, idxs in by_cat.items():
        mean = embeddings[idxs].mean(axis=0)
        norm = np.linalg.norm(mean)
        if norm > 0:
            mean /= norm
        centroids[cat] = mean
    return centroids


def classify_by_centroid(
    query_emb: np.ndarray,
    centroids: dict[str, np.ndarray],
) -> tuple[str, float, float]:
    """Returns (predicted_category, top1_score, margin).

    margin = top1_score - top2_score. High margin = confident.
    """
    scores = {cat: float(np.dot(query_emb, c)) for cat, c in centroids.items()}
    ranked = sorted(scores.items(), key=lambda x: -x[1])
    top1_cat, top1_score = ranked[0]
    top2_score = ranked[1][1] if len(ranked) > 1 else 0.0
    margin = top1_score - top2_score
    return top1_cat, top1_score, margin


def rule_based_classify(query: str) -> str:
    """Quick reimplementation of cqs's classify_query for comparison.
    Uses the same output as `cqs` would — calls cqs CLI."""
    try:
        r = subprocess.run(
            ["cqs", query, "--json", "--limit", "0"],
            capture_output=True, text=True, timeout=15,
        )
        if r.returncode == 0:
            data = json.loads(r.stdout)
            # cqs search output includes classification in some modes
            # For a clean comparison, use the llm_client.classify logic.
            pass
    except Exception:
        pass
    # Fallback: return "unknown" — we'll implement proper rule-based later
    # For now the experiment focuses on centroid-only vs ground truth.
    return "unknown"


def loocv(embeddings: np.ndarray, labels: list[str]) -> dict:
    """Leave-one-out cross-validation. For each query, rebuild centroids
    from the other N-1, classify, check against ground truth."""
    n = len(labels)
    correct = 0
    per_cat_correct: Counter = Counter()
    per_cat_total: Counter = Counter()
    margins: list[float] = []
    misclassifications: list[dict] = []

    for i in range(n):
        # Build centroids without query i.
        mask = np.ones(n, dtype=bool)
        mask[i] = False
        centroids = compute_centroids(embeddings[mask], [labels[j] for j in range(n) if j != i])
        pred, score, margin = classify_by_centroid(embeddings[i], centroids)
        true = labels[i]
        per_cat_total[true] += 1
        margins.append(margin)
        if pred == true:
            correct += 1
            per_cat_correct[true] += 1
        else:
            misclassifications.append({
                "query_idx": i,
                "true": true,
                "pred": pred,
                "score": round(score, 4),
                "margin": round(margin, 4),
            })

    return {
        "accuracy": correct / n,
        "correct": correct,
        "total": n,
        "per_category": {
            cat: {
                "accuracy": per_cat_correct[cat] / per_cat_total[cat] if per_cat_total[cat] else 0,
                "correct": per_cat_correct[cat],
                "total": per_cat_total[cat],
            }
            for cat in CATEGORIES
            if per_cat_total[cat] > 0
        },
        "margin_median": float(np.median(margins)),
        "margin_p10": float(np.percentile(margins, 10)),
        "margin_p90": float(np.percentile(margins, 90)),
        "misclassifications": misclassifications[:20],  # cap for readability
    }


def evaluate_split(
    embeddings: np.ndarray,
    labels: list[str],
    centroids: dict[str, np.ndarray],
    threshold: float = 0.0,
) -> dict:
    """Evaluate on a split. If margin < threshold, prediction is 'unknown'
    (simulating fallback to rule-based)."""
    correct = 0
    abstained = 0
    per_cat_correct: Counter = Counter()
    per_cat_total: Counter = Counter()

    for i in range(len(labels)):
        true = labels[i]
        per_cat_total[true] += 1
        pred, score, margin = classify_by_centroid(embeddings[i], centroids)
        if margin < threshold:
            abstained += 1
            continue  # would fall back to rule-based
        if pred == true:
            correct += 1
            per_cat_correct[true] += 1

    answered = len(labels) - abstained
    return {
        "accuracy_of_answered": correct / answered if answered else 0,
        "coverage": answered / len(labels),
        "correct": correct,
        "answered": answered,
        "abstained": abstained,
        "total": len(labels),
        "threshold": threshold,
        "per_category": {
            cat: {
                "accuracy": per_cat_correct[cat] / per_cat_total[cat] if per_cat_total[cat] else 0,
                "correct": per_cat_correct[cat],
                "total": per_cat_total[cat],
            }
            for cat in CATEGORIES
            if per_cat_total[cat] > 0
        },
    }


def threshold_sweep(
    embeddings: np.ndarray,
    labels: list[str],
    centroids: dict[str, np.ndarray],
) -> list[dict]:
    """Sweep confidence thresholds on dev to find the best accuracy/coverage tradeoff."""
    thresholds = [0.0, 0.01, 0.02, 0.03, 0.05, 0.07, 0.10, 0.15, 0.20, 0.30]
    return [evaluate_split(embeddings, labels, centroids, t) for t in thresholds]


def main() -> int:
    print("loading model...")
    from sentence_transformers import SentenceTransformer
    model = SentenceTransformer(MODEL_NAME)

    # Load splits.
    train = load_split(TRAIN_PATH)
    dev = load_split(DEV_PATH)
    print(f"train: {len(train)}  dev: {len(dev)}")
    print(f"train categories: {Counter(q['category'] for q in train).most_common()}")

    # Embed.
    print("\nembedding train queries...")
    train_queries = [q["query"] for q in train]
    train_labels = [q["category"] for q in train]
    t0 = time.monotonic()
    train_embs = embed_queries(train_queries, model)
    print(f"  {train_embs.shape} in {time.monotonic()-t0:.1f}s")

    print("embedding dev queries...")
    dev_queries = [q["query"] for q in dev]
    dev_labels = [q["category"] for q in dev]
    dev_embs = embed_queries(dev_queries, model)
    print(f"  {dev_embs.shape}")

    # LOOCV on train.
    print("\n=== LOOCV on train ===")
    loocv_result = loocv(train_embs, train_labels)
    print(f"accuracy: {loocv_result['accuracy']:.1%} ({loocv_result['correct']}/{loocv_result['total']})")
    print(f"margin: median={loocv_result['margin_median']:.4f} p10={loocv_result['margin_p10']:.4f} p90={loocv_result['margin_p90']:.4f}")
    print("per-category:")
    for cat, stats in sorted(loocv_result["per_category"].items(), key=lambda x: -x[1]["accuracy"]):
        print(f"  {cat:<22} {stats['accuracy']:5.1%} ({stats['correct']}/{stats['total']})")
    if loocv_result["misclassifications"]:
        print(f"\nfirst 10 misclassifications:")
        for m in loocv_result["misclassifications"][:10]:
            q = train_queries[m["query_idx"]]
            print(f"  [{m['true']:<20}→{m['pred']:<20}] margin={m['margin']:.4f} {q[:70]}")

    # Build final centroids from ALL train data.
    centroids = compute_centroids(train_embs, train_labels)
    print(f"\nbuilt {len(centroids)} centroids")

    # Dev evaluation (no threshold).
    print("\n=== Dev evaluation (no threshold) ===")
    dev_result = evaluate_split(dev_embs, dev_labels, centroids, threshold=0.0)
    print(f"accuracy: {dev_result['accuracy_of_answered']:.1%} ({dev_result['correct']}/{dev_result['total']})")
    print("per-category:")
    for cat, stats in sorted(dev_result["per_category"].items(), key=lambda x: -x[1]["accuracy"]):
        print(f"  {cat:<22} {stats['accuracy']:5.1%} ({stats['correct']}/{stats['total']})")

    # Threshold sweep on dev.
    print("\n=== Threshold sweep on dev ===")
    sweep = threshold_sweep(dev_embs, dev_labels, centroids)
    print(f"{'θ':>6}  {'acc':>6}  {'coverage':>8}  {'answered':>8}  {'abstain':>7}")
    for s in sweep:
        print(f"{s['threshold']:6.2f}  {s['accuracy_of_answered']:5.1%}  {s['coverage']:7.1%}  {s['answered']:>8}  {s['abstained']:>7}")

    # Save centroids.
    CENTROIDS_OUT.parent.mkdir(parents=True, exist_ok=True)
    centroid_data = {
        "model": MODEL_NAME,
        "prefix": BGE_QUERY_PREFIX,
        "dim": int(train_embs.shape[1]),
        "n_train": len(train),
        "created_at": int(time.time()),
        "categories": {
            cat: {"centroid": centroids[cat].tolist(), "n_train": sum(1 for l in train_labels if l == cat)}
            for cat in centroids
        },
        "loocv_accuracy": loocv_result["accuracy"],
        "dev_accuracy": dev_result["accuracy_of_answered"],
    }
    CENTROIDS_OUT.write_text(json.dumps(centroid_data, indent=2))
    print(f"\ncentroids saved to {CENTROIDS_OUT}")

    # Summary.
    print(f"\n{'='*60}")
    print(f"LOOCV accuracy (train): {loocv_result['accuracy']:.1%}")
    print(f"Dev accuracy (no θ)   : {dev_result['accuracy_of_answered']:.1%}")
    print(f"Centroids dim         : {train_embs.shape[1]}")
    print(f"Centroid file size    : {CENTROIDS_OUT.stat().st_size / 1024:.1f} KB")

    return 0


if __name__ == "__main__":
    sys.exit(main())
