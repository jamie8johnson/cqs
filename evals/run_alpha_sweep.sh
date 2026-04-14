#!/bin/bash
# Alpha sweep: run eval at each alpha via --splade-alpha flag.
# Works through daemon (3ms/query). No env var workarounds.
#
# 21-point sweep at 0.05 increments. Output goes to ~/.cache/cqs/evals/
# (outside watched project dir; eval json writes no longer invalidate the
# daemon's batch context between runs).

ALPHAS="0.00 0.05 0.10 0.15 0.20 0.25 0.30 0.35 0.40 0.45 0.50 0.55 0.60 0.65 0.70 0.75 0.80 0.85 0.90 0.95 1.00"

for alpha in $ALPHAS; do
    echo "=========================================="
    echo "  Alpha = $alpha"
    echo "=========================================="
    python3 evals/run_ablation.py --config bge-large --split all --splade-alpha "$alpha" 2>&1 | tee "/tmp/eval_alpha_${alpha}.log"
    echo ""
done

echo "Sweep complete. Results in ~/.cache/cqs/evals/run_*/"
