#!/usr/bin/env python3
"""
Real codebase eval — runs manual, call graph, and git blame queries against cqs index.
Outputs structured JSON with per-query results and aggregate metrics.

Usage:
    python3 scripts/run_real_eval.py [--json output.json] [--cqs-binary path]
"""

import json, subprocess, sys, os, argparse

def run_search(query, binary, limit=5, env=None):
    """Run cqs search and return parsed results."""
    try:
        out = subprocess.run(
            [binary, query, "--json", "-n", str(limit)],
            capture_output=True, text=True, timeout=30,
            env=env or os.environ,
        )
        data = json.loads(out.stdout)
        results = data if isinstance(data, list) else data.get("results", [])
        return [{"name": r.get("chunk", r).get("name", ""),
                 "score": round(r.get("score", 0), 4),
                 "file": r.get("chunk", r).get("origin", r.get("chunk", r).get("file", ""))}
                for r in results[:limit]]
    except Exception as e:
        return []


def eval_manual(queries_file, binary, env=None):
    """Evaluate manual queries (expected function name)."""
    data = json.load(open(queries_file))
    queries = data["queries"]
    results = []

    for q in queries:
        all_valid = [q["expected"]] + q.get("also_accept", [])
        top5 = run_search(q["query"], binary, env=env)

        rank = None
        for i, r in enumerate(top5):
            if rank is None and r["name"] in all_valid:
                rank = i + 1

        status = "+" if rank == 1 else ("~" if rank and rank <= 5 else "-")
        results.append({"type": "manual", "query": q["query"], "expected": q["expected"],
                       "rank": rank, "status": status, "top5": top5})
        print(f"  {status} [manual] \"{q['query'][:50]}\" -> rank={rank or 'miss'}")

    return results


def eval_callgraph(queries_file, binary, env=None):
    """Evaluate call graph queries (expected callers)."""
    data = json.load(open(queries_file))
    queries = data["queries"]
    results = []

    for q in queries:
        top5 = run_search(q["query"], binary, limit=10, env=env)
        top_names = {r["name"] for r in top5}
        expected = set(q["expected_callers"])

        overlap = top_names & expected
        precision = len(overlap) / len(top_names) if top_names else 0
        recall = len(overlap) / len(expected) if expected else 0

        status = "+" if recall >= 0.4 else ("~" if recall > 0 else "-")
        results.append({"type": "callgraph", "query": q["query"], "target": q["target"],
                       "expected_count": len(expected), "found": len(overlap),
                       "precision": round(precision, 3), "recall": round(recall, 3),
                       "status": status})
        print(f"  {status} [callgraph] \"{q['query'][:50]}\" -> {len(overlap)}/{len(expected)} callers found")

    return results


def eval_gitblame(queries_file, binary, env=None):
    """Evaluate git blame queries (expected files)."""
    data = json.load(open(queries_file))
    queries = data["queries"]
    results = []

    for q in queries:
        top5 = run_search(q["query"], binary, env=env)
        top_files = {r.get("file", "").replace("\\", "/") for r in top5}
        expected_files = set(q["files"])

        # Check if any result file matches any expected file (substring match for paths)
        found = False
        for tf in top_files:
            for ef in expected_files:
                if ef in tf or tf.endswith(ef):
                    found = True
                    break

        status = "+" if found else "-"
        results.append({"type": "gitblame", "query": q["query"][:80], "commit": q["commit"],
                       "expected_files": q["files"], "found_file_match": found,
                       "status": status, "top5_names": [r["name"] for r in top5[:3]]})
        print(f"  {status} [git] \"{q['query'][:50]}\" -> file_match={found}")

    return results


def eval_conceptual(queries, binary, env=None):
    """Evaluate conceptual queries (multiple expected functions, any match is good)."""
    results = []

    for q in queries:
        top5 = run_search(q["query"], binary, env=env)
        top_names = [r["name"] for r in top5]
        expected = set(q["expected_functions"])

        found = [n for n in top_names if n in expected]
        coverage = len(found) / len(expected) if expected else 0

        # "good" = at least one expected function in top 5
        status = "+" if found else "-"
        results.append({"type": "conceptual", "query": q["query"],
                       "category": q.get("category", ""),
                       "expected_count": len(expected), "found": found,
                       "coverage": round(coverage, 3), "status": status,
                       "top5": top_names})
        print(f"  {status} [concept] \"{q['query'][:50]}\" -> {len(found)}/{len(expected)} ({','.join(found[:3]) or 'miss'})")

    return results


def main():
    parser = argparse.ArgumentParser(description="Real codebase eval")
    parser.add_argument("--json", default=None, help="Output JSON file")
    parser.add_argument("--cqs-binary", default="cqs", help="Path to cqs binary")
    args = parser.parse_args()

    binary = args.cqs_binary
    all_results = []

    # Manual queries (original format)
    if os.path.exists("tests/real_eval_cqs.json"):
        print("\n=== Manual Queries (50q) ===")
        all_results.extend(eval_manual("tests/real_eval_cqs.json", binary))

    # Expanded eval — function lookup + conceptual
    expanded_path = "tests/real_eval_expanded.json"
    if os.path.exists(expanded_path):
        data = json.load(open(expanded_path))

        # Function lookup (same schema as manual)
        fn_queries = data.get("function_lookup", [])
        if fn_queries:
            print(f"\n=== Function Lookup ({len(fn_queries)}q) ===")
            # Wrap in {"queries": [...]} format for eval_manual
            import tempfile
            with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as tmp:
                json.dump({"queries": fn_queries}, tmp)
                tmp_path = tmp.name
            fn_results = eval_manual(tmp_path, binary)
            # Retype to distinguish from original manual
            for r in fn_results:
                r["type"] = "function_lookup"
            all_results.extend(fn_results)
            os.unlink(tmp_path)

        # Conceptual queries
        concept_queries = data.get("conceptual", [])
        if concept_queries:
            print(f"\n=== Conceptual Queries ({len(concept_queries)}q) ===")
            all_results.extend(eval_conceptual(concept_queries, binary))

    # Call graph queries
    if os.path.exists("tests/real_eval_callgraph.json"):
        print("\n=== Call Graph Queries ===")
        all_results.extend(eval_callgraph("tests/real_eval_callgraph.json", binary))

    # Git blame queries
    if os.path.exists("tests/real_eval_gitblame.json"):
        print("\n=== Git Blame Queries ===")
        all_results.extend(eval_gitblame("tests/real_eval_gitblame.json", binary))

    # Aggregate
    manual = [r for r in all_results if r["type"] == "manual"]
    fn_lookup = [r for r in all_results if r["type"] == "function_lookup"]
    conceptual = [r for r in all_results if r["type"] == "conceptual"]
    cg = [r for r in all_results if r["type"] == "callgraph"]
    git = [r for r in all_results if r["type"] == "gitblame"]

    print(f"\n{'='*60}")
    print(f"Real Codebase Eval Summary ({len(all_results)} queries)")
    for label, group in [("Manual", manual), ("Fn Lookup", fn_lookup)]:
        if group:
            hits = sum(1 for r in group if r["status"] == "+")
            r5 = sum(1 for r in group if r["status"] in ("+", "~"))
            print(f"  {label:12s} R@1={hits/len(group)*100:.1f}%  R@5={r5/len(group)*100:.1f}%  ({len(group)}q)")
    if conceptual:
        good = sum(1 for r in conceptual if r["status"] == "+")
        avg_cov = sum(r["coverage"] for r in conceptual) / len(conceptual)
        print(f"  {'Conceptual':12s} {good}/{len(conceptual)} good (≥1 match), avg_coverage={avg_cov:.2f}")
    if cg:
        good = sum(1 for r in cg if r["status"] == "+")
        avg_recall = sum(r["recall"] for r in cg) / len(cg)
        print(f"  {'CallGraph':12s} {good}/{len(cg)} good (≥40% recall), avg_recall={avg_recall:.2f}")
    if git:
        found = sum(1 for r in git if r["status"] == "+")
        print(f"  {'GitBlame':12s} {found}/{len(git)} file matches ({found/len(git)*100:.1f}%)")

    # Combined lookup R@1 (manual + fn_lookup)
    all_lookup = manual + fn_lookup
    if all_lookup:
        hits = sum(1 for r in all_lookup if r["status"] == "+")
        r5 = sum(1 for r in all_lookup if r["status"] in ("+", "~"))
        print(f"\n  Combined lookup: R@1={hits/len(all_lookup)*100:.1f}%  R@5={r5/len(all_lookup)*100:.1f}%  ({len(all_lookup)}q)")

    if args.json:
        with open(args.json, "w") as f:
            json.dump({"total": len(all_results), "results": all_results}, f, indent=2)
        print(f"\nSaved to {args.json}")


if __name__ == "__main__":
    main()
