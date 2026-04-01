#!/bin/bash
set -euo pipefail

# Cross-run stability test — runs pipeline_eval N times, reports median + variance.
# Usage: ./scripts/eval_stability.sh [runs] [model_env_args]
# Example: ./scripts/eval_stability.sh 5
# Example: ./scripts/eval_stability.sh 5 CQS_ONNX_DIR=/path/to/model CQS_EMBEDDING_MODEL=custom

RUNS=${1:-5}
shift 2>/dev/null || true

DIR="$(cd "$(dirname "$0")/.." && pwd)"
RESULTS_DIR="/tmp/eval_stability_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

echo "Cross-run stability test: $RUNS runs"
echo "Results dir: $RESULTS_DIR"
echo "Extra env: $*"
echo "=========================================="

R1_VALUES=()
MRR_VALUES=()

for i in $(seq 1 "$RUNS"); do
    OUTPUT="$RESULTS_DIR/run_${i}.json"
    echo -n "Run $i/$RUNS... "

    env "$@" CQS_EVAL_OUTPUT="$OUTPUT" \
        cargo test --features gpu-index --test pipeline_eval test_pipeline_scoring \
        -- --nocapture --ignored 2>/dev/null

    R1=$(python3 -c "import json; print(json.load(open('$OUTPUT'))['metrics']['recall_at_1'])")
    MRR=$(python3 -c "import json; print(json.load(open('$OUTPUT'))['metrics']['mrr'])")
    R1_VALUES+=("$R1")
    MRR_VALUES+=("$MRR")
    echo "R@1=$R1 MRR=$MRR"
done

echo ""
echo "=========================================="
echo "Results:"

python3 << PYEOF
import json, statistics, sys, os

runs = []
for i in range(1, $RUNS + 1):
    path = f"$RESULTS_DIR/run_{i}.json"
    runs.append(json.load(open(path)))

r1s = [r['metrics']['recall_at_1'] * 100 for r in runs]
mrrs = [r['metrics']['mrr'] for r in runs]

print(f"  R@1: median={statistics.median(r1s):.1f}% mean={statistics.mean(r1s):.1f}% stdev={statistics.stdev(r1s):.2f}pp range={min(r1s):.1f}-{max(r1s):.1f}%")
print(f"  MRR: median={statistics.median(mrrs):.4f} mean={statistics.mean(mrrs):.4f} stdev={statistics.stdev(mrrs):.4f}")

# Per-query agreement: how many queries get the same result across all runs?
all_queries = runs[0]['queries']
agreed = 0
disagreed = []
for qi in range(len(all_queries)):
    statuses = set()
    for r in runs:
        statuses.add(r['queries'][qi].get('status', '?'))
    if len(statuses) == 1:
        agreed += 1
    else:
        disagreed.append((all_queries[qi]['query'], all_queries[qi]['language'], statuses))

print(f"\n  Per-query agreement: {agreed}/{len(all_queries)} ({100*agreed/len(all_queries):.1f}%)")
if disagreed:
    print(f"  Unstable queries ({len(disagreed)}):")
    for q, lang, statuses in disagreed[:10]:
        print(f"    [{lang}] \"{q}\" -> {statuses}")

# Save summary
summary = {
    'runs': $RUNS,
    'r1_values': r1s,
    'mrr_values': mrrs,
    'r1_median': statistics.median(r1s),
    'r1_stdev': statistics.stdev(r1s),
    'mrr_median': statistics.median(mrrs),
    'mrr_stdev': statistics.stdev(mrrs),
    'agreement': agreed,
    'total_queries': len(all_queries),
    'unstable_queries': [(q, l, list(s)) for q, l, s in disagreed],
}
with open(f"$RESULTS_DIR/summary.json", 'w') as f:
    json.dump(summary, f, indent=2)
print(f"\nSummary saved to $RESULTS_DIR/summary.json")
PYEOF
