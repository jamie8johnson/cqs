# Project Continuity

## Right Now

**Hard negative mining running, v7 training pipeline ready (2026-03-22 04:53 CDT).**

### Running
- Mining 1.89M combined pairs (9 languages) with GPU FAISS. Corpus embedding 23% at 3 it/s. ~2 hours remaining, then FAISS search + sampling.

### Ready when mining completes
```bash
python train_lora.py \
  --data combined_9lang_hard_negs.jsonl \
  --output ./e5-code-search-lora-v7 \
  --epochs 1 --use-gist --matryoshka --export-onnx
```
New in v7: GISTEmbedLoss (false negative filtering via guide model), Matryoshka (multi-dim 768/384/192/128), hard negatives (CoRNStack recipe), 9 languages.

### Uncommitted cqs changes
- `PROJECT_CONTINUITY.md`, `ROADMAP.md`, `docs/research-log.md` — literature survey, training plan, CoIR comparison

### Completed this session
- v1.3.0 released + 75 audit fixes merged (PRs #640-644, #651)
- HuggingFace v5 model + model card
- Full 10-task CoIR controlled comparison: base 49.47 vs v5 48.67
- Stack extraction: Rust 56k, TS 58k, C++ 63k (all pass consistency filter at 100%)
- Combined 9-language dataset: 1.89M pairs
- Private backup: github.com/jamie8johnson/cqs-training
- Python scripts quality pass (13 scripts: softmax overflow, error handling, observability)
- GitHub issues #645-650 for unfixed audit items
- Usage telemetry implemented (CQS_TELEMETRY=1)
- Literature survey: Qodo synthetic queries, GitHub hard negs, Matryoshka, GISTEmbedLoss, theoretical limits, dead ends
- train_lora.py updated: --use-gist, --matryoshka, --matryoshka-dims, --guide-model, --gist-margin

### Key findings
- **LoRA specialization trade-off confirmed**: v5 wins 3/9 CoIR tasks, loses 6 (CCR -7.9pp worst)
- **MNR loss may cause the degradation**: HF blog shows MNR alone hurts Korean retrieval by same magnitude. GISTEmbedLoss fixes it.
- **Hard negatives validated**: GitHub Copilot's biggest quality gain (+37.6%). CoRNStack +9.4pp. Both use InfoNCE with hard negs.
- **Synthetic queries work**: Qodo 1.5B beats 7B via LLM-generated training queries
- **Embeddings have theoretical limits**: sign-rank bounds on single-vector retrieval (arXiv 2508.21038)
- **Language-specific adapters reassessed**: our data sizes (56-63k) are 3.5x above LoRACode's best case — still viable after hard negs
- **Novel unexplored ideas**: negative space training, multi-granularity semantic embedding, co-evolution signal

## Parked
- Commit + PR the doc/roadmap updates
- Rebuild cqs binary + reindex + re-run --improve-all (after mining)
- Synthetic query augmentation ($3 Haiku batch — do before v7 training)
- Structural metadata in training NL (tree-sitter features — do before v7 training)
- Paper draft
- Balanced vs unbalanced training experiment (v7a vs v7b)

## Architecture
- Version: 1.3.0
- Schema: v16
- Embeddings: 768-dim E5-base-v2 LoRA v5 (166k/1ep)
- Metrics: 92.7% R@1, 0.965 NDCG@10 (hard eval, DocFirst)
- CoIR: base 49.47, v5 48.67 (9 tasks, controlled)
- Tests: 1290 lib pass
- Telemetry: CQS_TELEMETRY=1
