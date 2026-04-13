#!/bin/bash
# Full alpha sweep: run eval at each alpha, save results.
# We have alpha=1.0 (baseline) and alpha=0.7 (--splade always-on).
# This script runs remaining points via CQS_SPLADE_ALPHA env var
# (no --splade flag — goes through the per-category routing path).
#
# Prior 0.5/0.9 runs were INVALID: SPLADE model loading was gated on
# cli.splade not use_splade. Fixed in commit 0d66701.
#
# Order: 0.5 first to verify the fix works (should differ from baseline),
# then 0.9 to bracket, then fill inward.

# No set -e: one failed alpha shouldn't kill the sweep

ALPHAS="0.5 0.9 0.8 0.3 0.6 0.4 0.1 0.2 0.0"

for alpha in $ALPHAS; do
    echo "=========================================="
    echo "  Alpha = $alpha"
    echo "=========================================="
    # BatchRunner sets CQS_NO_DAEMON=1 internally so batch process reads
    # CQS_SPLADE_ALPHA from its own env.
    CQS_SPLADE_ALPHA="$alpha" python3 evals/run_ablation.py --config bge-large --split all 2>&1 | tee "/tmp/eval_alpha_${alpha}.log"
    echo ""
done

echo "Sweep complete. Results in evals/runs/run_*/"
