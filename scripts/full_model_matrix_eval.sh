#!/usr/bin/env bash
# Full model matrix evaluation — ALL evals, ALL models, both GPUs.
#
# Phase 1: Hard eval (Rust, 3x per model per GPU) — ~15 min
# Phase 2: Enriched hard eval (with contrastive summaries) — ~5 min
# Phase 3: Full-pipeline eval (live index search) — ~5 min
# Phase 4: CoIR benchmarks (Python, per model) — ~2h per model
#
# Usage:
#   bash scripts/full_model_matrix_eval.sh           # Phase 1-3 only
#   bash scripts/full_model_matrix_eval.sh --coir    # All phases including CoIR
#
# CoIR requires: conda activate cqs-train && cd ~/training-data

set -euo pipefail
cd /mnt/c/Projects/cqs

OUTFILE="eval_matrix_$(date +%Y%m%d_%H%M%S).txt"
DO_COIR="${1:-}"

echo "=== Full Model Matrix Eval — $(date) ===" | tee "$OUTFILE"
echo "Host: $(hostname), GPUs: $(nvidia-smi -L 2>/dev/null | wc -l)" | tee -a "$OUTFILE"
nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null | tee -a "$OUTFILE"

# =========================================================
# Phase 1: Hard eval — all models, 3x per GPU
# =========================================================
run_hard_eval() {
    local gpu_id="$1"
    local gpu_name="$2"
    echo "" | tee -a "$OUTFILE"
    echo "========== PHASE 1: Hard Eval — $gpu_name (device $gpu_id) ==========" | tee -a "$OUTFILE"

    for run in 1 2 3; do
        echo "" | tee -a "$OUTFILE"
        echo "--- Run $run/3 ---" | tee -a "$OUTFILE"
        CUDA_VISIBLE_DEVICES=$gpu_id cargo test --features gpu-index --test model_eval \
            -- test_hard_model_comparison --ignored --nocapture 2>&1 | \
            grep -E "^Model|^E5-" | tee -a "$OUTFILE"
    done
}

echo ""
echo "Phase 1: Hard eval (3x, A6000 only)..."
run_hard_eval 0 "A6000"

# =========================================================
# Phase 2: Enriched hard eval — with contrastive summaries
# =========================================================
echo "" | tee -a "$OUTFILE"
echo "========== PHASE 2: Enriched Hard Eval (with summaries) ==========" | tee -a "$OUTFILE"

echo "" | tee -a "$OUTFILE"
echo "--- A6000 ---" | tee -a "$OUTFILE"
CUDA_VISIBLE_DEVICES=0 cargo test --features gpu-index --test model_eval \
    -- test_hard_with_summaries --ignored --nocapture 2>&1 | \
    grep -E "Recall|NDCG|Coverage|Chunks|summaries" | tee -a "$OUTFILE"

# =========================================================
# Phase 3: Full-pipeline eval (live index, fixture-scoped)
# =========================================================
echo "" | tee -a "$OUTFILE"
echo "========== PHASE 3: Full-Pipeline Eval (live index) ==========" | tee -a "$OUTFILE"

# Rebuild binary first to ensure latest code
cargo build --release --features gpu-index 2>&1 | tail -1
systemctl --user stop cqs-watch 2>/dev/null || true
cp ~/.cargo-target/cqs/release/cqs ~/.cargo/bin/cqs
systemctl --user start cqs-watch 2>/dev/null || true

# Reindex to ensure fresh embeddings
cqs index 2>&1 | tail -3 | tee -a "$OUTFILE"

# Run full-pipeline eval 3x
for run in 1 2 3; do
    echo "" | tee -a "$OUTFILE"
    echo "--- Full-pipeline run $run/3 ---" | tee -a "$OUTFILE"
    bash tests/full_pipeline_eval.sh 2>&1 | grep -E "=== Results|Recall|NDCG" | tee -a "$OUTFILE"
done

# =========================================================
# Phase 4: CoIR benchmarks (optional, slow)
# =========================================================
if [ "$DO_COIR" = "--coir" ]; then
    echo "" | tee -a "$OUTFILE"
    echo "========== PHASE 4: CoIR Benchmarks ==========" | tee -a "$OUTFILE"
    echo "NOTE: Each model takes ~30 min for CSN, ~2h for full 9-task." | tee -a "$OUTFILE"
    echo "" | tee -a "$OUTFILE"

    COIR_DIR="$HOME/training-data"
    COIR_PYTHON="$HOME/miniforge3/envs/cqs-train/bin/python"

    for model_name in "base" "lora-v5" "lora-v7" "lora-v7b" "lora-v8-keydac"; do
        echo "--- CoIR: $model_name ---" | tee -a "$OUTFILE"
        if [ -f "$COIR_DIR/run_coir.py" ]; then
            CUDA_VISIBLE_DEVICES=0 $COIR_PYTHON "$COIR_DIR/run_coir.py" \
                --model "$model_name" --all 2>&1 | \
                grep -E "NDCG@10|Overall|Task" | tee -a "$OUTFILE"
        else
            echo "  SKIP: run_coir.py not found at $COIR_DIR" | tee -a "$OUTFILE"
        fi
    done
fi

echo "" | tee -a "$OUTFILE"
echo "=== Complete — $(date) ===" | tee -a "$OUTFILE"
echo ""
echo "Results saved to: $OUTFILE"
echo ""
echo "To run CoIR benchmarks separately:"
echo "  bash scripts/full_model_matrix_eval.sh --coir"
