#!/usr/bin/env python3
"""Generate eval queries from the cqs index.

Reads chunks from the SQLite index, generates categorized queries,
validates against the search API, and outputs v2_300q.json.

Usage: python3 evals/generate_queries.py
"""

import json
import random
import sqlite3
import subprocess
import sys
from pathlib import Path

DB_PATH = Path(".cqs/index.db")
OUTPUT = Path("evals/queries/v2_300q.json")

# Category targets (aim for ~40 each, 320 total, trim to 300)
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

def get_chunks(db_path):
    """Load interesting chunks from the index."""
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row

    # Get non-test, non-doc source chunks with names
    rows = conn.execute("""
        SELECT DISTINCT name, origin, chunk_type, language, line_start, line_end,
               substr(content, 1, 200) as preview
        FROM chunks
        WHERE name != ''
          AND origin NOT LIKE 'tests/%'
          AND origin NOT LIKE 'docs/%'
          AND origin LIKE 'src/%'
          AND chunk_type NOT IN ('section', 'module', 'configkey')
        ORDER BY origin, line_start
    """).fetchall()
    conn.close()
    return [dict(r) for r in rows]


def get_test_chunks(db_path):
    """Load test function chunks."""
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    rows = conn.execute("""
        SELECT DISTINCT name, origin, chunk_type, language, line_start
        FROM chunks
        WHERE name LIKE 'test_%'
          AND chunk_type = 'function'
          AND origin LIKE 'src/%'
        ORDER BY RANDOM()
        LIMIT 50
    """).fetchall()
    conn.close()
    return [dict(r) for r in rows]


def validate_query(query_text, expected_name, acceptable_names=None, n=20):
    """Check if expected_name appears in top-n search results."""
    try:
        result = subprocess.run(
            ["cqs", query_text, "--json", "-n", str(n)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30
        )
        data = json.loads(result.stdout)
        names = [r["name"] for r in data.get("results", [])]
        all_acceptable = [expected_name] + (acceptable_names or [])

        for i, name in enumerate(names):
            if name in all_acceptable:
                return i + 1  # rank (1-indexed)
        return None  # not found
    except Exception:
        return None


def dedupe_by_name(chunks):
    """Keep one chunk per unique name, preferring src/ files."""
    seen = {}
    for c in chunks:
        name = c["name"]
        if name not in seen or (c["origin"].startswith("src/") and not seen[name]["origin"].startswith("src/")):
            seen[name] = c
    return list(seen.values())


def generate_identifier_queries(chunks, target=45):
    """Generate identifier lookup queries — literal name searches."""
    # Pick diverse functions/structs/enums across modules
    by_type = {}
    for c in chunks:
        ct = c["chunk_type"]
        if ct in ("function", "method", "struct", "enum", "trait", "constant"):
            by_type.setdefault(ct, []).append(c)

    queries = []
    selected = set()

    for ct, pool in by_type.items():
        random.shuffle(pool)
        suffix = {"function": "function", "method": "method", "struct": "struct",
                  "enum": "enum", "trait": "trait", "constant": "constant"}
        for c in pool:
            if len(queries) >= target:
                break
            if c["name"] in selected:
                continue
            # Skip very short names (likely ambiguous)
            if len(c["name"]) < 4:
                continue

            query_text = f"{c['name']}"
            rank = validate_query(query_text, c["name"])
            if rank and rank <= 5:
                selected.add(c["name"])
                queries.append({
                    "query": query_text,
                    "category": "identifier_lookup",
                    "primary_answer": {"name": c["name"], "file": c["origin"]},
                    "rank": rank,
                })
                if len(queries) % 10 == 0:
                    print(f"  identifier: {len(queries)}/{target}", file=sys.stderr)

    return queries[:target]


def generate_type_filtered_queries(chunks, target=40):
    """Generate type-filtered queries — chunk type or file module scoped."""
    queries = []

    # Group chunks by module (first two path segments)
    by_module = {}
    for c in chunks:
        parts = c["origin"].split("/")
        if len(parts) >= 2:
            module = "/".join(parts[:2])
            by_module.setdefault(module, []).append(c)

    # "error types in X" queries
    error_chunks = [c for c in chunks if "error" in c["name"].lower() or c["chunk_type"] == "enum" and "Error" in c["name"]]
    for c in error_chunks[:8]:
        module = c["origin"].rsplit("/", 1)[0]
        query = f"error types in {module}"
        rank = validate_query(query, c["name"])
        if rank and rank <= 20:
            queries.append({
                "query": query,
                "category": "type_filtered",
                "primary_answer": {"name": c["name"], "file": c["origin"]},
                "rank": rank,
            })

    # "struct definitions in X" queries
    struct_chunks = [c for c in chunks if c["chunk_type"] == "struct"]
    by_dir = {}
    for c in struct_chunks:
        d = c["origin"].rsplit("/", 1)[0]
        by_dir.setdefault(d, []).append(c)

    for d, sc in sorted(by_dir.items(), key=lambda x: -len(x[1]))[:10]:
        c = sc[0]
        query = f"struct definitions in {d}"
        rank = validate_query(query, c["name"], [s["name"] for s in sc[:3]])
        if rank and rank <= 20:
            queries.append({
                "query": query,
                "category": "type_filtered",
                "primary_answer": {"name": c["name"], "file": c["origin"]},
                "acceptable_answers": [{"name": s["name"], "file": s["origin"]} for s in sc[1:3]],
                "rank": rank,
            })

    # "constants in X" queries
    const_chunks = [c for c in chunks if c["chunk_type"] == "constant"]
    for c in const_chunks[:8]:
        query = f"constants defined in {c['origin']}"
        rank = validate_query(query, c["name"])
        if rank and rank <= 20:
            queries.append({
                "query": query,
                "category": "type_filtered",
                "primary_answer": {"name": c["name"], "file": c["origin"]},
                "rank": rank,
            })

    # "trait definitions" queries
    trait_chunks = [c for c in chunks if c["chunk_type"] == "trait"]
    for c in trait_chunks[:8]:
        query = f"{c['name']} trait definition"
        rank = validate_query(query, c["name"])
        if rank and rank <= 10:
            queries.append({
                "query": query,
                "category": "type_filtered",
                "primary_answer": {"name": c["name"], "file": c["origin"]},
                "rank": rank,
            })

    if len(queries) % 10 == 0:
        print(f"  type_filtered: {len(queries)}/{target}", file=sys.stderr)

    return queries[:target]


def generate_structural_queries(chunks, target=40):
    """Generate structural search queries."""
    queries = []

    # "methods on X struct"
    struct_names = set(c["name"] for c in chunks if c["chunk_type"] == "struct")
    method_chunks = [c for c in chunks if c["chunk_type"] == "method"]

    # Group methods by likely parent struct (from file proximity)
    for struct_name in list(struct_names)[:15]:
        struct_chunk = next((c for c in chunks if c["name"] == struct_name and c["chunk_type"] == "struct"), None)
        if not struct_chunk:
            continue
        nearby_methods = [c for c in method_chunks if c["origin"] == struct_chunk["origin"]]
        if nearby_methods:
            m = nearby_methods[0]
            query = f"methods on {struct_name}"
            rank = validate_query(query, m["name"], [x["name"] for x in nearby_methods[:3]])
            if rank and rank <= 20:
                queries.append({
                    "query": query,
                    "category": "structural_search",
                    "primary_answer": {"name": m["name"], "file": m["origin"]},
                    "acceptable_answers": [{"name": x["name"], "file": x["origin"]} for x in nearby_methods[1:3]],
                    "rank": rank,
                })

    # "impl blocks for X"
    for struct_name in list(struct_names)[:15]:
        query = f"implementation of {struct_name}"
        rank = validate_query(query, struct_name)
        if rank and rank <= 10:
            queries.append({
                "query": query,
                "category": "structural_search",
                "primary_answer": {"name": struct_name, "file": next(c["origin"] for c in chunks if c["name"] == struct_name)},
                "rank": rank,
            })

    print(f"  structural: {len(queries)}/{target}", file=sys.stderr)
    return queries[:target]


def assign_splits(queries, held_out_ratio=1/3):
    """Assign train/held_out splits."""
    random.shuffle(queries)
    n_held_out = int(len(queries) * held_out_ratio)
    for i, q in enumerate(queries):
        q["split"] = "held_out" if i < n_held_out else "train"
    return queries


def build_query_set(generated, existing_path):
    """Merge generated queries with hand-curated ones."""
    # Load existing hand-curated queries
    with open(existing_path) as f:
        existing = json.load(f)

    hand_curated = existing["queries"]
    hand_ids = {q["id"] for q in hand_curated}

    # Add generated queries with new IDs
    all_queries = list(hand_curated)
    counters = {}

    for q in generated:
        cat_prefix = {
            "identifier_lookup": "id",
            "behavioral_search": "beh",
            "conceptual_search": "con",
            "type_filtered": "tf",
            "cross_language": "cl",
            "structural_search": "st",
            "negation": "neg",
            "multi_step": "ms",
        }[q["category"]]

        counters[cat_prefix] = counters.get(cat_prefix, 100) + 1
        qid = f"{cat_prefix}-{counters[cat_prefix]:03d}"

        entry = {
            "id": qid,
            "query": q["query"],
            "category": q["category"],
            "tags": q.get("tags", []),
            "language": q.get("language"),
            "primary_answer": q["primary_answer"],
            "acceptable_answers": q.get("acceptable_answers", []),
            "negative_examples": q.get("negative_examples", []),
            "split": q.get("split", "train"),
        }
        all_queries.append(entry)

    return {
        "version": "v2_300q",
        "created": "2026-04-06",
        "description": f"Eval query set ({len(all_queries)} queries, 8 categories) against cqs codebase. Hand-curated + auto-generated, ground truth validated.",
        "queries": all_queries,
    }


def main():
    random.seed(42)  # Reproducible

    print("Loading chunks from index...", file=sys.stderr)
    chunks = get_chunks(DB_PATH)
    chunks = dedupe_by_name(chunks)
    print(f"  {len(chunks)} unique named chunks", file=sys.stderr)

    generated = []

    print("\nGenerating identifier queries...", file=sys.stderr)
    id_queries = generate_identifier_queries(chunks, target=35)
    generated.extend(id_queries)
    print(f"  → {len(id_queries)} identifier queries", file=sys.stderr)

    print("\nGenerating type-filtered queries...", file=sys.stderr)
    tf_queries = generate_type_filtered_queries(chunks, target=30)
    generated.extend(tf_queries)
    print(f"  → {len(tf_queries)} type-filtered queries", file=sys.stderr)

    print("\nGenerating structural queries...", file=sys.stderr)
    st_queries = generate_structural_queries(chunks, target=25)
    generated.extend(st_queries)
    print(f"  → {len(st_queries)} structural queries", file=sys.stderr)

    # Assign splits to generated queries
    generated = assign_splits(generated)

    print(f"\nTotal generated: {len(generated)}", file=sys.stderr)
    print(f"Merging with hand-curated queries...", file=sys.stderr)

    query_set = build_query_set(generated, OUTPUT)

    # Write output
    with open(OUTPUT, "w") as f:
        json.dump(query_set, f, indent=2)

    # Summary
    by_cat = {}
    by_split = {"train": 0, "held_out": 0}
    for q in query_set["queries"]:
        by_cat[q["category"]] = by_cat.get(q["category"], 0) + 1
        by_split[q["split"]] = by_split.get(q["split"], 0) + 1

    print(f"\nOutput: {OUTPUT}", file=sys.stderr)
    print(f"Total: {len(query_set['queries'])} queries", file=sys.stderr)
    print(f"Split: {by_split['train']} train, {by_split['held_out']} held-out", file=sys.stderr)
    print("Categories:", file=sys.stderr)
    for cat in sorted(by_cat):
        print(f"  {cat}: {by_cat[cat]}", file=sys.stderr)


if __name__ == "__main__":
    main()
