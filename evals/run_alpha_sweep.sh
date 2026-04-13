#!/bin/bash
# Alpha sweep: run eval at each alpha via --splade-alpha flag.
# Works through daemon (3ms/query). No env var workarounds.

ALPHAS="0.5 0.9 0.8 0.3 0.6 0.4 0.1 0.2 0.0"

for alpha in $ALPHAS; do
    echo "=========================================="
    echo "  Alpha = $alpha"
    echo "=========================================="
    python3 evals/run_ablation.py --config bge-large --split all --splade-alpha "$alpha" 2>&1 | tee "/tmp/eval_alpha_${alpha}.log"
    echo ""
done

echo "Sweep complete. Results in evals/runs/run_*/"
