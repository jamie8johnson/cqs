# cqs Wiki System Design

A persistent, compounding knowledge base for code research — maintained by agents, queryable via cqs.

---

## Overview

The cqs wiki is a structured directory of markdown files that accumulates synthesized knowledge across sessions. Unlike chat history that gets lost on compaction, or notes.toml that holds raw observations, the wiki maintains a curated view that agents can query, update, and build on.

**Primary consumers are agents.** Every query, every finding, every experiment result should be one `cqs wiki` call away from any session. The wiki is an agent's long-term memory — not a human reading tool with agent write access bolted on.

The wiki is a standalone git repo — not colocated inside any single codebase. Research spans multiple projects, and colocating pollutes code search and mixes commit history.

---

## Architecture

```
~/wiki/                        ← standalone git repo
  index.md                     ← content catalog (machine-readable YAML sections)
  log.md                       ← append-only activity log (rotated)
  log-archive.md               ← rotated log entries
  schema.md                    ← conventions and workflows
  experiments/                 ← one page per training run or trial
  techniques/                  ← methods, losses, strategies
  models/                      ← models evaluated
  datasets/                    ← training and eval datasets
  findings/                    ← synthesized insights (claims with evidence)
  papers/                      ← literature summaries
  evals/                       ← benchmark definitions and results
```

**Indexed by cqs** as a reference project: `cqs ref add wiki ~/wiki/`. This makes wiki pages searchable from any project via `--ref wiki` or `cqs wiki` commands.

---

## Agent Interface

### `cqs wiki <query>` — Search wiki

Semantic search scoped to the wiki reference. Returns pages ranked by relevance.

```bash
cqs wiki "SPLADE training results"          # search wiki
cqs wiki "best embedding model" --json      # JSON output for batch mode
cqs wiki "reranker" --gather                # search + follow cross-references
```

In batch mode: `wiki "SPLADE training"` → JSON results from wiki reference.

### `cqs wiki add <category> <title>` — Create/update page

File a finding, experiment, or technique directly from a session.

```bash
# File a finding
cqs wiki add finding "SPLADE null on code search" \
  --confidence low \
  --evidence "v2 eval 75q, 0pp delta on BGE-large and E5-LoRA" \
  --related "splade-training,bge-large-baseline"

# File an experiment
cqs wiki add experiment "v2-eval-ablation" \
  --status production \
  --results "BGE-large 68% R@1, E5-LoRA 55%" \
  --finding "BGE-large dominates diverse queries"

# File a technique
cqs wiki add technique "embedding-cache" \
  --status production \
  --implemented-in "src/cache.rs"
```

The command generates the page from the template, adds cross-references, updates index.md, appends to log.md, and commits.

### `cqs wiki update <page>` — Update existing page

Revise a finding, add contradicting evidence, change status.

```bash
cqs wiki update findings/splade-null --confidence high \
  --note "Confirmed across 4 configs in v2 eval ablation"
```

### `cqs wiki lint` — Validate wiki health

Check for stale claims, orphan pages, broken links, missing cross-references.

```bash
cqs wiki lint              # report issues
cqs wiki lint --fix        # auto-fix what's fixable (broken links, missing index entries)
```

### `cqs wiki graduate` — Promote notes to wiki pages

Notes in `docs/notes.toml` that meet graduation criteria are promoted:

- Sentiment >= 0.5 or <= -0.5, AND age > 7 days
- Referenced in 3+ search results within a session
- Contradicts an existing wiki finding

```bash
cqs wiki graduate          # promote eligible notes
cqs wiki graduate --dry-run  # preview without writing
```

---

## Page Formats

Each category has a template. YAML frontmatter enables structured filtering by agents.

### Finding page

```markdown
---
status: production        # production | rejected | superseded | speculative
confidence: high          # high | medium | low | speculative
established: 2026-04-07
tags: [search, splade, eval]
---

# SPLADE is null on code search

**Implication:** Don't ship off-the-shelf SPLADE. Need code-trained variant.

## The finding

Off-the-shelf naver/splade-cocondenser-ensembledistil adds 0pp R@1 on BGE-large
and -1.4pp on E5-LoRA (v2 eval, 75 train queries, 4 configs tested).

## Evidence
- [v2-eval-ablation](../experiments/v2-eval-ablation.md): 4-config matrix
- [reranker-ablation](../experiments/reranker-ablation.md): also null

## Caveats
- Web-trained model, not code-trained. Code-fine-tuned SPLADE might help.
- N=75, CI ~±6pp. Need 300q eval for per-category confidence.

## Contradictions
- None yet.

## Related
- [embedding-enrichment-tradeoff](embedding-enrichment-tradeoff.md)
```

### Experiment page

```markdown
---
status: production
date: 2026-04-07
model: BGE-large
eval: v2-75q
---

# V2 Eval Dense × Sparse Ablation

## Configuration
- Dense models: BGE-large (1024d), E5-LoRA v9-200k (768d)
- Sparse modes: none, SPLADE (naver/splade-cocondenser)
- 75 train queries, 8 categories, live cqs index (~13k chunks)

## Results

| Config | R@1 | R@5 | R@20 |
|--------|-----|-----|------|
| BGE-large | 68.0% | 86.7% | 98.7% |
| BGE-large + SPLADE | 68.0% | 86.7% | 98.7% |
| E5-LoRA v9-200k | 54.7% | 76.0% | 97.3% |
| E5-LoRA v9-200k + SPLADE | 53.3% | 76.0% | 97.3% |

## Key finding
See: [splade-null](../findings/splade-null.md), [bge-dominates](../findings/bge-dominates.md)
```

### Technique page

```markdown
---
status: production
implemented_in: [src/cache.rs, src/cli/commands/infra/cache_cmd.rs]
---

# Embedding Cache

## What it does
SQLite cache at ~/.cache/cqs/embeddings.db keyed by (content_hash, model_fingerprint).

## Why it matters
Avoids ONNX re-inference for unchanged chunks across reindexes and model switches.

## Evidence
- [model-switch-test](../experiments/model-switch-test.md): 8,287 entries, 2 models, 81 MB
```

---

## Cross-Reference Convention

All cross-references use relative markdown links: `[page-name](../category/page-name.md)`.

Not `[[wiki-links]]` — cqs can't resolve those during `gather` traversal. Relative links are machine-traversable edges.

---

## Operations

### Ingest (new experiment result)

1. Create/update experiment page in `experiments/`
2. Update technique pages the result informs
3. Update finding pages confirmed, contradicted, or refined
4. Update `index.md`
5. Append to `log.md`
6. Git commit: `wiki: ingest | <experiment-name>`

### Contradiction handling

1. Update the finding page — revise claim, note what changed
2. Update experiment page that established old claim — add "superseded"
3. Follow cross-references, update citing pages
4. Log: `wiki: contradiction | <brief>`

### Lint

Periodic validation:
1. Stale claims — findings contradicted by newer experiments
2. Orphan pages — no inbound links
3. Missing pages — concepts mentioned but no page
4. Broken links
5. Missing cross-references
6. Log rotation (>100 entries → archive)

---

## Log Format

`log.md` is append-only. Entries:

```markdown
## [2026-04-07] ingest | v2-eval-ablation
Created experiments/v2-eval-ablation.md. Updated findings/splade-null.md, findings/bge-dominates.md.
```

Verbs: `bootstrap`, `ingest`, `graduate`, `lint`, `contradiction`, `query`, `update`.

Rotation: at 100 entries, move all but last 50 to `log-archive.md` (excluded from cqs index via `.cqsignore`).

---

## index.md Format

Machine-readable catalog with YAML-style sections:

```markdown
# Wiki Index

Last updated: 2026-04-07

## Findings (N)
- [splade-null](findings/splade-null.md) — Off-the-shelf SPLADE adds 0pp on code search
- [bge-dominates](findings/bge-dominates.md) — BGE-large +13pp over E5-LoRA on diverse queries

## Experiments (N)
- [v2-eval-ablation](experiments/v2-eval-ablation.md) — Dense × sparse × reranker matrix
```

---

## schema.md

Operational reference (the wiki's CLAUDE.md). Documents:
- Page templates per category
- Linking conventions
- Contradiction procedure
- Status/confidence vocabulary
- Frontmatter conventions
- Git commit format: `wiki: <verb> | <description>`
- Graduation criteria
- Category definitions

---

## Implementation Plan

1. **Create repo + scaffold** — `~/wiki/`, git init, directory structure, schema.md, index.md
2. **`cqs wiki` command** — search, add, update, lint, graduate subcommands
3. **Register as reference** — `cqs ref add wiki ~/wiki/`
4. **Bootstrap** — file current session's findings as seed content
5. **Batch integration** — `wiki` command in batch mode for agent access
6. **Skills** — `/wiki-update`, `/wiki-lint`, `/wiki-graduate`

---

## Appendix: Human Browsing

The wiki is markdown files in a git repo. Any tool that renders markdown works for reading.

**Obsidian** is the best fit for browsing — graph view shows structure (hubs, orphans, clusters), relative links are clickable, and the Dataview plugin can query YAML frontmatter (e.g., "all findings with confidence: Low").

**Setup:** Open `~/wiki/` as an Obsidian vault. Pages are immediately navigable. Optional: Dataview plugin for dynamic filtered tables over frontmatter fields.

The browsing interface is not the editing interface. Humans read; agents write via `cqs wiki` commands.
