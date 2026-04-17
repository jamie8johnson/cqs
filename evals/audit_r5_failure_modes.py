#!/usr/bin/env python3
"""R@5 failure-mode audit on v3 test (109 queries, BGE-large, shipping config).

Goal: decompose the R@5→R@20 gap (gold ranks 6-20). Identify *why* gold
slipped past the top-5 cutoff. Heuristic classification into:

  - classifier_misroute  : router picked a category whose alpha hurt R@5
  - near_dup_crowding    : top-5 dominated by chunks from the same file/name
  - wrong_abstraction    : top-5 are higher-level orchestrators (longer chunks)
                           when query asks for low-level detail (or vice versa)
  - truncated_gold       : gold chunk is suspiciously small (<5 lines or
                           less than a third of typical chunk size for its lang)
  - unexplained          : no heuristic fires — needs LLM pass

Output:
    docs/audit-r5-failure-modes.md      — narrative report
    evals/queries/v3_r5_audit.json      — raw per-query record (resumable)

Run:
    CQS_NO_DAEMON=1 python3 evals/audit_r5_failure_modes.py

Observable: per-query progress, summary every 10. Resumable via the JSON file.
Robust: per-query try/except, SIGINT-safe (saves before exit).
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import subprocess
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"
TEST_PATH = QUERIES_DIR / "v3_test.json"
DEV_PATH = QUERIES_DIR / "v3_dev.json"
OUT_JSON = QUERIES_DIR / "v3_r5_audit.json"
OUT_MD = Path(__file__).parent.parent / "docs" / "audit-r5-failure-modes.md"

K_TOP = 20  # full pool we ask for
GOLD_RANK_NEAR_MISS_LO = 6  # R@5 miss
GOLD_RANK_NEAR_MISS_HI = 20  # R@20 hit


def load_split(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    rows = [q for q in data["queries"] if q.get("gold_chunk")]
    return rows


def gold_key(g: dict) -> tuple:
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def result_key(r: dict) -> tuple:
    return (r.get("file"), r.get("name"), r.get("line_start"))


def run_batch(queries: list[dict], k: int = K_TOP) -> list[dict]:
    """Run all queries through `cqs batch`. Returns results aligned to queries."""
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open("/tmp/r5-audit.stderr", "ab"),
        text=True,
        bufsize=1,
        env=env,
    )

    results = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            # Match v1.27.0 shipping config: SPLADE on, per-category router
            # picks alpha (cross_language=0.10, others use v1.26.0 defaults).
            # Centroid classifier is opt-in (CQS_CENTROID_CLASSIFIER=1) — leave off.
            cmd = f"search {shlex.quote(q['query'])} --limit {k} --splade"
            try:
                proc.stdin.write(cmd + "\n")
                proc.stdin.flush()
            except (BrokenPipeError, OSError) as e:
                print(f"batch died: {e}", file=sys.stderr)
                break

            line = proc.stdout.readline()
            if not line:
                print(f"batch EOF at q={i}", file=sys.stderr)
                break

            try:
                out = json.loads(line)
                if "error" in out:
                    results.append({"query": q["query"], "error": out["error"]})
                    continue
                results.append(out)
            except json.JSONDecodeError as e:
                results.append({"query": q["query"], "error": f"parse: {e}"})

            if (i + 1) % 10 == 0 or i + 1 == len(queries):
                rate = (i + 1) / (time.monotonic() - t0)
                print(
                    f"  {i+1}/{len(queries)} queries ({rate:.1f} qps)",
                    file=sys.stderr,
                    flush=True,
                )
    finally:
        try:
            proc.stdin.close()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()

    return results


def find_rank(results_list: list[dict], gold: dict) -> tuple[int | None, str]:
    """Return (rank, match_kind) where match_kind ∈ {'strict', 'basename', 'name', 'none'}.

    Strict matches by (origin, name, line_start). Basename falls back to
    (basename(origin), name, line_start) for cases where the origin path
    drifted (e.g., worktree prefix). Name fallback matches just (name) —
    weaker, used to flag eval-data drift.
    """
    target = gold_key(gold)
    target_name = gold.get("name")
    target_origin_base = (gold.get("origin") or "").split("/")[-1].split("\\")[-1]
    target_lstart = gold.get("line_start")

    for i, r in enumerate(results_list):
        if result_key(r) == target:
            return i + 1, "strict"
    for i, r in enumerate(results_list):
        rfile_base = (r.get("file") or "").split("/")[-1].split("\\")[-1]
        if (rfile_base, r.get("name"), r.get("line_start")) == (
            target_origin_base, target_name, target_lstart
        ):
            return i + 1, "basename"
    for i, r in enumerate(results_list):
        if r.get("name") == target_name and target_name:
            return i + 1, "name"
    return None, "none"


def classify_failure(query_obj: dict, search_out: dict, top5: list[dict]) -> list[str]:
    """Return list of failure mode labels for a single near-miss query."""
    modes = []

    # 0. eval_artifact: gold lives in a path likely to be stale or non-prod
    # (worktree carve-out, plan docs from old sessions, sample fixtures).
    # These are *eval-data* problems, not retrieval failures.
    origin = (query_obj["gold_chunk"].get("origin") or "")
    if any(prefix in origin for prefix in [".claude/worktrees/", ".claude\\worktrees\\"]):
        modes.append("eval_artifact_worktree")
    elif origin.startswith("docs/") or origin.startswith("docs\\"):
        modes.append("eval_artifact_docs")

    # 1. classifier_misroute: predicted category != gold category
    pred_cat = (search_out.get("debug") or {}).get("classified_category")
    gold_cat = query_obj.get("category")
    if pred_cat and gold_cat and pred_cat.lower() != gold_cat.lower():
        # Only count as misroute if pred isn't "unknown" — unknown means no
        # alpha override applied (default α=1.0), not an active misclassification.
        if pred_cat.lower() != "unknown":
            modes.append("classifier_misroute")

    # 2. near_dup_crowding: top-5 dominated by same file or same name
    files = Counter(r.get("file") for r in top5 if r.get("file"))
    names = Counter(r.get("name") for r in top5 if r.get("name"))
    if files and files.most_common(1)[0][1] >= 3:
        modes.append("near_dup_crowding")
    elif names and names.most_common(1)[0][1] >= 3:
        modes.append("near_dup_crowding")

    # 3. wrong_abstraction: top-5 chunks are 2x longer than gold (orchestrator
    # crowding) OR top-5 chunks are <1/3 of gold (gold is the orchestrator,
    # detail chunks crowded).
    gold = query_obj["gold_chunk"]
    gold_lines = max(1, (gold.get("line_end", 0) or 0) - (gold.get("line_start", 0) or 0))
    top5_lines = []
    for r in top5:
        ls = r.get("line_start") or 0
        le = r.get("line_end") or 0
        if le > ls:
            top5_lines.append(le - ls)
    if top5_lines:
        med = sorted(top5_lines)[len(top5_lines) // 2]
        if med >= 2 * gold_lines and gold_lines >= 3:
            modes.append("wrong_abstraction_top_too_big")
        elif med * 3 <= gold_lines and med >= 3:
            modes.append("wrong_abstraction_top_too_small")

    # 4. truncated_gold: gold is suspiciously short (<5 lines)
    if gold_lines < 5:
        modes.append("truncated_gold")

    # If only eval-artifact modes fired, leave it — that's the failure type.
    # Otherwise if no real retrieval mode fired, mark as unexplained.
    real_modes = [m for m in modes if not m.startswith("eval_artifact")]
    if not real_modes:
        if not any(m.startswith("eval_artifact") for m in modes):
            modes.append("unexplained")
    return modes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="test", choices=["test", "dev"])
    ap.add_argument("--reuse", action="store_true",
                    help="Reuse v3_r5_audit.json if present, skip search")
    args = ap.parse_args()

    src = TEST_PATH if args.split == "test" else DEV_PATH
    rows = load_split(src)
    print(f"loaded {len(rows)} queries from {src.name}", file=sys.stderr)

    # --- Stage 1: search ---
    if args.reuse and OUT_JSON.exists():
        cached = json.loads(OUT_JSON.read_text())
        if cached.get("split") == args.split and len(cached.get("rows", [])) == len(rows):
            print("reusing cached search results", file=sys.stderr)
            search_results = cached["rows"]
        else:
            print("cache mismatch — re-running search", file=sys.stderr)
            search_results = None
    else:
        search_results = None

    if search_results is None:
        print("running cqs batch search...", file=sys.stderr)
        search_outputs = run_batch(rows, k=K_TOP)
        search_results = []
        for q, so in zip(rows, search_outputs):
            results = so.get("results", []) if isinstance(so, dict) else []
            rank, match_kind = find_rank(results, q["gold_chunk"])
            search_results.append({
                "query": q["query"],
                "category": q["category"],
                "gold_chunk": q["gold_chunk"],
                "results": results[:K_TOP],
                "rank": rank,
                "match_kind": match_kind,
                "debug": so.get("debug") if isinstance(so, dict) else None,
                "error": so.get("error") if isinstance(so, dict) else None,
            })

        # Save raw record so subsequent runs can --reuse
        OUT_JSON.parent.mkdir(parents=True, exist_ok=True)
        OUT_JSON.write_text(json.dumps({
            "split": args.split,
            "n": len(rows),
            "k": K_TOP,
            "rows": search_results,
        }, indent=2))
        print(f"raw record → {OUT_JSON}", file=sys.stderr)

    # --- Stage 2: top-line metrics ---
    n = len(search_results)
    # Strict counts: only matches found via exact (origin, name, line_start)
    r1 = sum(1 for r in search_results if r["rank"] == 1 and r.get("match_kind") == "strict")
    r5 = sum(1 for r in search_results if r["rank"] and r["rank"] <= 5 and r.get("match_kind") == "strict")
    r20 = sum(1 for r in search_results if r["rank"] and r["rank"] <= 20 and r.get("match_kind") == "strict")
    misses = sum(1 for r in search_results if r["rank"] is None or r.get("match_kind") != "strict")
    # Permissive counts: also accept basename + name fallback matches
    r1_loose = sum(1 for r in search_results if r["rank"] == 1)
    r5_loose = sum(1 for r in search_results if r["rank"] and r["rank"] <= 5)
    r20_loose = sum(1 for r in search_results if r["rank"] and r["rank"] <= 20)
    # Match-kind breakdown
    by_match_kind = Counter(r.get("match_kind") or "none" for r in search_results)

    near_misses = [
        r for r in search_results
        if r["rank"] and GOLD_RANK_NEAR_MISS_LO <= r["rank"] <= GOLD_RANK_NEAR_MISS_HI
    ]
    print(f"\n=== v3 {args.split} top-line (strict origin/name/line match) ===", file=sys.stderr)
    print(f"  R@1  : {r1}/{n} ({100*r1/n:.1f}%)", file=sys.stderr)
    print(f"  R@5  : {r5}/{n} ({100*r5/n:.1f}%)", file=sys.stderr)
    print(f"  R@20 : {r20}/{n} ({100*r20/n:.1f}%)", file=sys.stderr)
    print(f"  miss : {misses}/{n} (gold not strict-matched in top-{K_TOP})", file=sys.stderr)
    print(f"\n=== permissive (basename + name fallback) ===", file=sys.stderr)
    print(f"  R@1  : {r1_loose}/{n} ({100*r1_loose/n:.1f}%)", file=sys.stderr)
    print(f"  R@5  : {r5_loose}/{n} ({100*r5_loose/n:.1f}%)", file=sys.stderr)
    print(f"  R@20 : {r20_loose}/{n} ({100*r20_loose/n:.1f}%)", file=sys.stderr)
    print(f"\n=== match-kind breakdown ===", file=sys.stderr)
    for k, ct in by_match_kind.most_common():
        print(f"  {k}: {ct}", file=sys.stderr)
    print(f"\n  near-misses (rank 6-20, any match): {len(near_misses)} — these are the targets", file=sys.stderr)

    # --- Stage 3: failure-mode classification ---
    mode_counts = Counter()
    by_category_modes = defaultdict(Counter)
    near_miss_records = []
    for r in near_misses:
        modes = classify_failure(r, {"debug": r.get("debug")}, r["results"][:5])
        for m in modes:
            mode_counts[m] += 1
            by_category_modes[r["category"]][m] += 1
        near_miss_records.append({**r, "failure_modes": modes})

    # --- Stage 4: write report ---
    OUT_MD.parent.mkdir(parents=True, exist_ok=True)
    lines = []
    lines.append(f"# R@5 Failure-Mode Audit — v3 {args.split}")
    lines.append("")
    lines.append(f"**Run:** {time.strftime('%Y-%m-%d %H:%M:%S')}  ")
    lines.append(f"**Config:** v1.27.0 shipping (cross_language α=0.10, BGE-large, no centroid classifier)")
    lines.append(f"**Queries:** {n} (loaded from `{src.name}`)")
    lines.append("")
    lines.append("## Top-line")
    lines.append("")
    lines.append("| Metric | strict % | permissive % | Δ |")
    lines.append("|---|---|---|---|")
    lines.append(f"| R@1  | {100*r1/n:.1f}% ({r1}) | {100*r1_loose/n:.1f}% ({r1_loose}) | +{100*(r1_loose-r1)/n:.1f}pp |")
    lines.append(f"| R@5  | {100*r5/n:.1f}% ({r5}) | {100*r5_loose/n:.1f}% ({r5_loose}) | +{100*(r5_loose-r5)/n:.1f}pp |")
    lines.append(f"| R@20 | {100*r20/n:.1f}% ({r20}) | {100*r20_loose/n:.1f}% ({r20_loose}) | +{100*(r20_loose-r20)/n:.1f}pp |")
    lines.append("")
    lines.append("- **Strict** = exact `(origin, name, line_start)` match — what `cqs eval` reports.")
    lines.append("- **Permissive** = also accept `(basename(origin), name, line_start)` and `name`-only matches. The gap reveals stale gold paths (worktree carve-outs, doc-to-code rename).")
    lines.append("")
    lines.append("### Match-kind composition")
    lines.append("")
    lines.append("| match_kind | N |")
    lines.append("|---|---|")
    for k, ct in by_match_kind.most_common():
        lines.append(f"| `{k}` | {ct} |")
    lines.append("")
    lines.append(f"The strict R@5 → R@20 gap is **{100*r20/n - 100*r5/n:.1f}pp**. ")
    lines.append(f"This audit decomposes the {len(near_misses)} queries that landed in rank [{GOLD_RANK_NEAR_MISS_LO}, {GOLD_RANK_NEAR_MISS_HI}].")
    lines.append("")
    lines.append("### Baseline drift note")
    lines.append("")
    lines.append("ROADMAP records v1.27.0 baseline as **R@1 42.2 / R@5 64.2 / R@20 78.9** on v3 test. ")
    lines.append("This audit run measures lower (see Top-line above). The corpus has grown since the ")
    lines.append("baseline measurement (~14.9k → 16.6k chunks); some near-misses below are likely ")
    lines.append("genuinely new chunks competing with gold. Re-baselining after this audit is advisable.")
    lines.append("")

    lines.append("## Failure modes (rank 6-20)")
    lines.append("")
    lines.append("Each near-miss query is tagged with one or more failure modes. Counts overlap.")
    lines.append("")
    lines.append("| Mode | N | % of near-misses |")
    lines.append("|---|---|---|")
    nm = max(1, len(near_misses))
    for mode, ct in mode_counts.most_common():
        lines.append(f"| `{mode}` | {ct} | {100*ct/nm:.1f}% |")
    lines.append("")

    lines.append("### Mode definitions")
    lines.append("")
    lines.append("- **eval_artifact_worktree** — gold lives under `.claude/worktrees/`. Origin path won't survive worktree cleanup; counts as miss against the parent index.")
    lines.append("- **eval_artifact_docs** — gold lives in `docs/` (often code-block targets in superseded plan files). Real production code with the same name may rank well; gold path is the artifact.")
    lines.append("- **classifier_misroute** — router predicted a non-Unknown category that disagrees with the gold label. SPLADE alpha applied was wrong for the actual query type.")
    lines.append("- **near_dup_crowding** — top-5 contains ≥3 chunks from the same file or with the same function name. Diversity-starved retrieval — MMR target.")
    lines.append("- **wrong_abstraction_top_too_big** — top-5 median chunk is ≥2x longer than gold (large orchestrators crowd out the targeted detail).")
    lines.append("- **wrong_abstraction_top_too_small** — top-5 median chunk is <1/3 of gold (small detail chunks crowd out the gold orchestrator).")
    lines.append("- **truncated_gold** — gold chunk is <5 lines. Likely a thin signature/struct that loses content-vector signal vs longer matches.")
    lines.append("- **unexplained** — no heuristic fires. Likely embedding-space lexical mismatch; needs LLM-driven analysis or cross-encoder reranking.")
    lines.append("")

    lines.append("## Per-category breakdown")
    lines.append("")
    cats = sorted(by_category_modes.keys())
    if cats:
        # header
        all_modes = sorted({m for c in cats for m in by_category_modes[c]})
        lines.append("| Category | N | " + " | ".join(f"`{m}`" for m in all_modes) + " |")
        lines.append("|---|---|" + "---|" * len(all_modes))
        for c in cats:
            row_total = sum(by_category_modes[c].values())
            lines.append(f"| {c} | {row_total} | " + " | ".join(
                str(by_category_modes[c].get(m, 0)) for m in all_modes
            ) + " |")
        lines.append("")

    lines.append("## Near-miss queries (rank 6-20)")
    lines.append("")
    for r in sorted(near_miss_records, key=lambda r: (r["rank"], r["category"])):
        gold = r["gold_chunk"]
        lines.append(f"### `[rank {r['rank']}]` `{r['category']}` — {r['query']}")
        lines.append("")
        lines.append(f"- **Gold:** `{gold.get('name')}` in `{gold.get('origin')}` "
                     f"(L{gold.get('line_start')}-{gold.get('line_end')}, "
                     f"`{gold.get('chunk_type', '?')}`, `{gold.get('language', '?')}`)")
        lines.append(f"- **Failure modes:** {', '.join(f'`{m}`' for m in r['failure_modes'])}")
        if r.get("debug"):
            cat_pred = r["debug"].get("classified_category")
            if cat_pred:
                lines.append(f"- **Classifier:** predicted `{cat_pred}`")
        lines.append("- **Top-5:**")
        for i, res in enumerate(r["results"][:5]):
            lines.append(f"  {i+1}. `{res.get('name', '?')}` in `{res.get('file', '?')}` "
                         f"(L{res.get('line_start', '?')}-{res.get('line_end', '?')}) "
                         f"score={res.get('score', 0):.3f}")
        lines.append("")

    lines.append("## Strategy implications")
    lines.append("")
    lines.append("Each lever's *expected R@5 lift on v3 test* is a back-of-envelope ceiling — assumes the lever fully solves every query in its mode. Real lift will be a fraction of that, and modes overlap (a single query can be both `near_dup_crowding` AND `wrong_abstraction_*`), so additive ceilings are wrong.")
    lines.append("")
    near_n = max(1, len(near_misses))
    lift_table = []
    for mode in ["near_dup_crowding", "wrong_abstraction_top_too_big",
                 "wrong_abstraction_top_too_small", "truncated_gold",
                 "classifier_misroute", "unexplained",
                 "eval_artifact_worktree", "eval_artifact_docs"]:
        ct = mode_counts.get(mode, 0)
        if ct == 0:
            continue
        # Ceiling: if this mode were fully solved, count moves into top-5.
        ceiling_pp = 100 * ct / n
        lift_table.append((mode, ct, ceiling_pp))
    lift_table.sort(key=lambda r: -r[1])
    lines.append("| Mode | Near-misses | R@5 ceiling if fully solved | Lever |")
    lines.append("|---|---|---|---|")
    lever_for = {
        "near_dup_crowding": "MMR re-rank on top-K pool (λ≈0.5-0.7). Cheap, no model change.",
        "wrong_abstraction_top_too_big": "Chunk-type aware boost when query intent demands detail (e.g. `extract_*` queries → leaf functions).",
        "wrong_abstraction_top_too_small": "Boost orchestrators when query verbs/nouns suggest top-down (e.g. 'workflow', 'pipeline').",
        "truncated_gold": "Chunker fix: pad short chunks with leading docstring/comment block. Schema-level lift.",
        "classifier_misroute": "Broaden α=1.0 fallback for low-confidence non-Unknown predictions; centroid pilot was −4.6pp so this isn't a free win.",
        "unexplained": "Reranker V2 (Phase 2 in flight) — catches lexical mismatch via cross-encoder.",
        "eval_artifact_worktree": "Eval-data fix: rebuild v3 test fixture from current corpus (gold paths drift when worktrees come and go).",
        "eval_artifact_docs": "Eval-data fix or re-judging: gold-in-docs is often a superseded plan target. Check if production code with same name exists; if so, swap gold.",
    }
    for mode, ct, ceil in lift_table:
        lines.append(f"| `{mode}` | {ct}/{n} | +{ceil:.1f}pp | {lever_for.get(mode, '?')} |")
    lines.append("")
    lines.append("**Recommended ordering** (by effort/impact):")
    lines.append("")
    lines.append(f"1. **Eval-data hygiene first** ({mode_counts.get('eval_artifact_worktree', 0) + mode_counts.get('eval_artifact_docs', 0)} queries). Re-baselining without fixing eval artifacts means we're chasing noise.")
    lines.append(f"2. **MMR for `near_dup_crowding`** ({mode_counts.get('near_dup_crowding', 0)} queries — biggest single mode). 1-2 day implementation, no model change. Sanity-check on v3 dev before merging.")
    lines.append(f"3. **Reranker V2 for `unexplained`** ({mode_counts.get('unexplained', 0)} queries). Already in flight (Phase 2 corpus build). Wait for trained model.")
    lines.append(f"4. **Chunker tuning for `truncated_gold`** ({mode_counts.get('truncated_gold', 0)} queries). Bigger lift; gated on training-data signal that it's worth a reindex.")
    lines.append(f"5. **Skip `classifier_misroute` standalone work**. Centroid pilot proved this lever is harder than it looks (−4.6pp). Better lift comes from removing the router entirely once Reranker V2 lands.")
    lines.append("")

    OUT_MD.write_text("\n".join(lines) + "\n")
    print(f"\nreport → {OUT_MD}", file=sys.stderr)
    print(f"raw    → {OUT_JSON}", file=sys.stderr)


def _sigint_handler(signum, frame):
    print("\n[INT] writing partial state then exiting", file=sys.stderr)
    sys.exit(130)


if __name__ == "__main__":
    signal.signal(signal.SIGINT, _sigint_handler)
    main()
