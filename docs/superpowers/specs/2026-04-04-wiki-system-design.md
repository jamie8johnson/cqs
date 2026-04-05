# cqs Wiki System Design

A persistent, compounding knowledge base for code research — maintained by LLMs, searchable by cqs.

---

## Overview

The cqs wiki is a structured directory of markdown files that sits between raw research artifacts (papers, experiment logs, benchmark results) and the working context of any given session. Unlike RAG systems that re-derive knowledge from scratch on every query, the wiki accumulates and maintains a synthesized view that gets richer over time.

The wiki is a standalone project — not colocated inside any single codebase. Research spans multiple projects, and colocating the wiki with one repo conflates the tool with the research, pollutes code search results, and mixes wiki commit history with code commits. Instead, the wiki lives at its own path (e.g. `~/wiki/` or `~/training-data/wiki/`) and is registered as a cqs reference project via `cqs ref add wiki <path>`. This makes wiki pages searchable from any project when wanted, invisible when not.

**The key property:** every significant finding, decision, or synthesis is filed once and stays current. A new experiment that contradicts an old claim triggers an update to the relevant pages — not a note in a chat history that gets lost.

---

## Architecture

Three layers:

```
~/wiki/                        ← standalone git repo, LLM writes, human reads
  index.md                     ← content catalog
  log.md                       ← append-only activity log (rotated)
  log-archive.md               ← rotated log entries
  schema.md                    ← conventions and workflows
  experiments/                 ← one page per training run or trial
  techniques/                  ← methods, losses, strategies
  models/                      ← models evaluated
  datasets/                    ← training and eval datasets
  findings/                    ← synthesized insights
  papers/                      ← literature summaries
  evals/                       ← benchmark definitions and results

~/training-data/               ← raw sources (separate repo/directory)
  research_log.md              ← raw experiment log (immutable after filing)
  RESULTS.md                   ← raw eval numbers
  paper/                       ← paper drafts
```

**Raw sources** are immutable. The LLM reads them but never modifies them. The research log, raw benchmark results, and paper PDFs live in their own directory — not inside the wiki.

**The wiki** is entirely LLM-maintained. The human reads it; the LLM writes it. Cross-references use relative markdown links so cqs can traverse them during `gather`. All changes go through `/wiki-update` or `/wiki-lint` — never hand-edited. A pre-commit hook enforces this (see [Enforcement](#enforcement)).

**The schema** (`wiki/schema.md`) documents conventions so any session can pick up where the last one left off — page formats, linking conventions, update workflows, and what triggers a lint pass.

**Git tracking:** The wiki is its own git repository. Every `/wiki-update` and `/wiki-lint` pass ends with a git commit. This provides full history — `git log findings/some-finding.md` answers "what did we believe about X last month?" without snapshots or separate versioning. Keeping it separate from code repos means wiki commits don't pollute `git log` in your codebase.

**Categories are extensible.** The directories above are a starting point. A project studying compiler optimizations might add `benchmarks/` and `architectures/`. The schema documents whatever categories exist.

---

## Browsing

The LLM maintains the wiki; the human browses it in a tool optimized for reading. Any markdown viewer works. Obsidian is the best fit for a wiki this shape — its graph view shows structure at a glance (hubs, orphans, clusters), and relative markdown links are clickable navigation.

**Recommended setup:**
- Open the wiki directory as an Obsidian vault. Pages are immediately navigable.
- Graph view for structural health — orphan pages and disconnected clusters are visible before `/wiki-lint` reports them.
- Dataview plugin (optional) for dynamic queries over frontmatter. If wiki pages carry YAML frontmatter (status, date, confidence, source count), Dataview can generate filtered tables — e.g. "all findings with confidence: Low" or "experiments from the last 30 days."
- Obsidian Web Clipper (browser extension) for converting web articles and papers to markdown and dropping them into the raw sources directory.

**The browsing interface is not the editing interface.** The human reads in Obsidian; the LLM writes via skills. Obsidian is the IDE; the LLM is the programmer; the wiki is the codebase.

---

## Cross-Reference Convention

All cross-references use relative markdown links, not wiki-style `[[double-bracket]]` syntax. This ensures cqs can traverse links during `gather` and `related` operations.

**Format:** `[page-name](../category/page-name.md)`

**Examples:**
- From `experiments/run-a.md` → `See: [some-finding](../findings/some-finding.md)`
- From `findings/x.md` → `Evidence: [run-a](../experiments/run-a.md), [run-b](../experiments/run-b.md)`
- Within same directory → `[run-b](run-b.md)`

**Why not `[[wiki-links]]`:** cqs indexes markdown by headings and extracts cross-references from actual links. `[[wiki-links]]` are opaque strings — cqs can't resolve them to the linked page during `gather` traversal. Relative markdown links are machine-traversable edges, not just human-readable annotations.

**In `index.md`:** entries use full relative paths from wiki root:
```markdown
- [run-a](experiments/run-a.md) — One-line summary of what this experiment tested
```

---

## Page Formats

Each category has a template. Templates live in `wiki/schema.md` and are the reference for any session creating pages.

### Experiment page

```markdown
# <experiment-name>

**Status:** Production / Rejected / Superseded / In Progress
**Date:** YYYY-MM-DD
**Training time:** (if applicable)

## Configuration
- (key parameters — data size, model, loss, hyperparameters)

## Results

| Eval | Metric 1 | Metric 2 | Metric 3 |
|------|----------|----------|----------|
| Eval A | — | — | — |
| Eval B | — | — | — |

## Key finding
(What this experiment proved, disproved, or revealed.)
See: [related-finding](../findings/related-finding.md), [technique-used](../techniques/technique-used.md)

## Variants
- [variant-a](variant-a.md) — what changed, what happened

## Contradictions / open questions
- (What this result conflicts with, what remains unexplained)
```

### Technique page

```markdown
# <technique-name>

**Status:** Production / Rejected / Experimental
**Novel:** Yes / No — (prior art note)
**Implemented in:** `file.py`, `module.rs`

## What it does
(Concise description of the technique.)

## Why it matters
- (Evidence for/against, with links to experiment pages)

## Mechanism
(Implementation detail — pseudocode, SQL, algorithm sketch.)

## Evidence
(Links to experiments that tested this technique.)
See: [experiment-a](../experiments/experiment-a.md), [finding-x](../findings/finding-x.md)

## Future
- (Planned extensions or open questions)
```

### Finding page

```markdown
# <finding — stated as a claim>

**Established:** YYYY-MM-DD (source experiment or analysis)
**Confidence:** High / Medium / Low / Speculative
**Implication:** (One-line practical consequence)

## The finding
(The claim, the evidence, the magnitude.)

## Caveats
- (Conditions under which the finding doesn't hold)

## Contradictions
- [other-finding](other-finding.md): (how it conflicts)

## Related
[finding-a](finding-a.md), [finding-b](finding-b.md)
```

### Paper page

```markdown
# <paper-title>

**Authors:** ...
**Venue:** ... (year)
**Relevance:** (Why this paper matters for this project)

## Thesis
(One paragraph.)

## Key findings
- (Bullet per finding, linked to relevant technique/finding pages)

## Applicability
(What we tried, what worked, what didn't — links to experiments)
```

---

## Operations

### Ingest (new experiment result)

1. Create or update the experiment page in `experiments/`
2. Update any technique pages that the result informs
3. Update any finding pages that are confirmed, contradicted, or refined
4. Update `index.md` if new pages were created
5. Append to `log.md`
6. `cd $CQS_WIKI_PATH && git add -A && git commit -m "wiki: ingest <experiment-name>"`

Trigger: manually after each experiment. Can be batched at end of session.

### Ingest (new paper)

1. Read the paper (or summary)
2. Create page in `papers/`
3. Update relevant technique pages with citations
4. Check findings pages for confirmation or contradiction
5. Update `index.md` and `log.md`
6. Git commit.

**Source preparation:** Web articles can be clipped to markdown via Obsidian Web Clipper or similar tools and dropped into `docs/`. PDFs can be converted via `cqs convert`. For sources with inline images, download images locally (Obsidian: Settings → Files and links → fixed attachment folder, then "Download attachments" hotkey) — this prevents broken URLs and lets the LLM reference images directly during ingest.

### Update (contradiction detected)

1. Update the finding page — revise the claim, note what changed and why
2. Update the experiment page that established the old claim — add a "superseded" note
3. Update any pages that cite the old claim (follow relative links to find them)
4. Append to `log.md` with a contradiction entry
5. Git commit with `wiki: contradiction — <brief description>`

### Query

`cqs gather "topic"` surfaces both wiki pages and implementation code in one call. No special workflow needed — the wiki is just markdown in the cqs index.

For synthesis questions ("what's the current best understanding of X"), read the relevant finding page directly. It's already synthesized.

Because cross-references are real relative links, `cqs gather` can follow them during BFS expansion — a search hit on one page can pull in linked pages automatically.

**Filing answers back:** Good query answers — comparisons, analyses, connections discovered during exploration — should be filed back into the wiki as new pages via `/wiki-update`. A comparison table you asked for shouldn't disappear into chat history. This is how queries compound: the wiki gets richer not just from ingested sources but from the questions asked against it.

### Lint

Run periodically (end of research phase, before paper writing):

1. Stale claims — findings contradicted by newer experiments
2. Orphan pages — no inbound links from other pages
3. Missing pages — concepts mentioned but lacking their own page
4. Broken links — relative links that point to nonexistent files
5. Missing cross-references — related pages not linked
6. Inconsistent results tables across pages
7. Log rotation — move older entries to `log-archive.md` if over threshold

### Graduate (notes.toml → wiki)

Notes in a project's `docs/notes.toml` that meet graduation criteria are promoted to wiki pages. Since the wiki is standalone, graduation crosses repo boundaries — the skill reads notes from the current project and writes pages to the wiki repo:

**Graduation triggers (any of):**
- Sentiment >= 0.5 or <= -0.5, AND age > 7 days (strong signal, not a flash reaction)
- Referenced in 3+ distinct search results within a session (recurring relevance)
- Manually tagged `wiki: true` in the note
- Contradicts an existing wiki finding (immediate promotion)

**Process:**
1. Check notes.toml for graduation candidates
2. Create or update the appropriate wiki page
3. Add `graduated: true` to the note (keeps it in notes.toml but marks it as filed)
4. Update `index.md` and `log.md`
5. Git commit

Notes that don't graduate remain in notes.toml indefinitely — they still surface via `cqs search` as contextual annotations. Graduation is promotion to a richer format, not deletion.

---

## Enforcement

The wiki's "LLM writes, human reads" contract requires enforcement. Without it, a hand-edit that breaks cross-references goes undetected until the next `/wiki-lint`.

**Pre-commit hook** (installed in the wiki repo):

```bash
#!/bin/bash
# Enforce wiki: prefix on all commits to the wiki repo.
# All modifications should go through /wiki-update or /wiki-lint.
MSG=$(cat .git/COMMIT_EDITMSG 2>/dev/null || echo "")
if ! echo "$MSG" | grep -q "^wiki:"; then
    echo "ERROR: Wiki commits must use 'wiki: <verb> | <description>' format."
    echo "Use /wiki-update or /wiki-lint to modify wiki pages."
    echo "Override: git commit --no-verify"
    exit 1
fi
```

**Escape hatch:** `--no-verify` for the rare case where a human needs to fix something directly. The next `/wiki-lint` re-validates regardless.

**Assumption:** Solo project. The hook is a speed bump, not a security boundary.

---

## Log Rotation

`log.md` is append-only but not unbounded.

**Threshold:** 100 entries. When exceeded, `/wiki-lint` moves all but the last 50 to `log-archive.md`.

`log-archive.md` is excluded from cqs indexing via `.cqsignore` — historical reference only.

**log.md entry format:**

```markdown
## [YYYY-MM-DD] verb | description
Pages affected. Details.
```

Verbs: `bootstrap`, `ingest`, `graduate`, `lint`, `contradiction`, `query`.

**Parse recent entries:** `grep "^## \[" wiki/log.md | tail -5`

---

## index.md Format

```markdown
# Wiki Index

Last updated: YYYY-MM-DD

## Experiments (N)
- [name](experiments/name.md) — one-line summary

## Techniques (N)
- [name](techniques/name.md) — one-line summary

## Models (N)
- [name](models/name.md) — one-line summary

## Datasets (N)
- [name](datasets/name.md) — one-line summary

## Findings (N)
- [name](findings/name.md) — one-line summary

## Papers (N)
- [name](papers/name.md) — one-line summary

## Evals (N)
- [name](evals/name.md) — one-line summary
```

---

## schema.md

The operational equivalent of CLAUDE.md for wiki maintenance. Documents:

- Page format templates for each category
- Linking conventions (relative markdown links, not `[[brackets]]`)
- Contradiction handling procedure
- Lint triggers and checklist
- Result table formats (consistent column order across pages)
- Status vocabulary: Production / Rejected / Superseded / Planned / In Progress
- Confidence vocabulary: High / Medium / Low / Speculative
- Graduation criteria for notes.toml → wiki
- Git commit message convention: `wiki: <verb> | <description>`
- Category definitions and when to create new categories
- Frontmatter conventions: YAML frontmatter on wiki pages (status, date, confidence, tags) enables Dataview queries in Obsidian and structured filtering by the LLM

Evolves as conventions stabilize. Early sessions should update it when they discover a convention that works.

---

## Bootstrap Procedure

Creates the initial wiki from existing research artifacts. Two-pass process.

**Input:** Research log (`~/training-data/research_log.md`), experiment records (`~/training-data/RESULTS.md`), notes from any project (`docs/notes.toml`).

### Pass 1: Stubs

1. Read the source material
2. Identify all entities: experiments, models, datasets, techniques, findings, papers, evals
3. Create stub pages: name, status, one-line summary, results table (numbers only)
4. Generate `index.md`
5. Write `schema.md` and initial `log.md` entry
6. Git commit: `wiki: bootstrap pass 1 | stubs for N entities`

Mechanical extraction. One medium session.

### Pass 2: Synthesis

1. Fill in cross-references between pages (relative links)
2. Write finding claims with evidence citations
3. Note contradictions and open questions
4. Verify all links resolve
5. Update `index.md` summaries
6. Git commit: `wiki: bootstrap pass 2 | cross-references and synthesis`

Requires reading multiple pages in context. One long session or two medium sessions.

### Pass 3 (optional): Audit

Run `/wiki-lint` on the fresh wiki. Fix orphans, broken links, inconsistencies.

---

## Skills

### `/wiki-bootstrap`
Two-pass bootstrap from research artifacts. Pass 1: stubs. Pass 2: cross-references and synthesis.
Input: path to source material. Output: populated wiki directory, git committed.

### `/wiki-update`
File a new result, paper, or finding. Also handles notes.toml graduation (`--graduate`). Updates relevant pages, checks for contradictions, updates index and log. Git commits with `wiki:` prefix.

### `/wiki-lint`
Health check. Stale claims, orphans, missing pages, broken links, inconsistent results. Rotates log.md if over threshold. Git commits fixes.

### `/wiki-query`
Synthesize an answer from wiki pages with citations. Good answers are optionally filed back as new finding pages.

---

## cqs Integration

The wiki is registered as a reference project: `cqs ref add wiki ~/wiki`. This makes wiki pages searchable alongside code via the existing multi-index search with weight-based merge.

**Setup:**
```bash
cd ~/wiki && cqs index          # index the wiki standalone
cd ~/projects/cqs && cqs ref add wiki ~/wiki  # register as reference
```

**Effects:**

- `cqs gather "topic"` returns wiki pages and implementation code together — multi-index merge ranks both by relevance
- `cqs "concept"` finds finding pages alongside code that implements the concept — wiki results appear with `source: "wiki"` attribution
- `cqs notes` and wiki are complementary: notes are short, sentiment-weighted, indexed immediately; wiki pages are structured, cross-referenced, updated deliberately
- `cqs health` / `cqs suggest` operate on code; `/wiki-lint` operates on the wiki — separate loops, separate repos
- `cqs related` discovers co-referenced wiki pages through shared link targets

**Index hygiene:** `log-archive.md` excluded via `.cqsignore` in the wiki repo. Default heading-based markdown chunking works without special config.

**Wiki-aware skills** need the wiki path. Convention: `CQS_WIKI_PATH` env var or `.cqs.toml` setting. Skills read this to know where to write pages. Falls back to `~/wiki` if unset.

---

## Connection to AutoDream / KAIROS

KAIROS is an unreleased daemon mode in Claude Code — an always-on background agent with a heartbeat loop that takes proactive actions during user idle time. AutoDream is its memory consolidation subsystem. Both were revealed in the Claude Code v2.1.88 source leak (March 31, 2026) — feature-flagged, built, not shipped.

AutoDream's four-phase consolidation cycle maps directly to wiki operations:

| AutoDream Phase | Wiki Operation | Skill |
|-----------------|---------------|-------|
| **Orient** — scan memory state, read index | Read `log.md` tail + `index.md` | `/wiki-lint` preamble |
| **Gather** — search for corrections, themes, decisions | `cqs gather` across wiki/ + check notes.toml graduation candidates | `/wiki-update --graduate` |
| **Consolidate** — merge observations, strengthen connections | Update finding pages, resolve contradictions, promote graduated notes | `/wiki-update` |
| **Prune** — remove stale content, keep index under cap | Remove stale claims, fix orphans, rotate log | `/wiki-lint` |

**Trigger conditions** (mirroring AutoDream's triple gate):
- AutoDream: 24+ hours since last consolidation AND 5+ new sessions
- Wiki equivalent: 7+ days since last lint AND (3+ new results OR 5+ new notes since last graduation)

When KAIROS ships, the transition is a trigger change, not an architecture change. Replace manual skill invocations with KAIROS idle-time triggers. The data model, operations, and cross-reference graph are already proven.

---

## Relationship to Tears

Tears handle session continuity — what's happening now, what's parked, what's blocking.

The wiki handles accumulated knowledge — what we've learned, what it means, how it connects.

| | Tears | Wiki |
|---|---|---|
| **Scope** | Current session state | Accumulated findings |
| **Time horizon** | This session / next session | Project lifetime |
| **Writer** | LLM (on trigger) | LLM (on trigger) |
| **Reader** | LLM (on resume) | LLM + human |
| **Update frequency** | Every session | Every significant finding |
| **Format** | Free-form state dump | Structured pages with cross-references |
| **cqs integration** | `notes.toml` indexed | Wiki directory indexed |
| **Versioning** | Overwritten each session | Git-tracked, full history |
| **KAIROS mapping** | AutoDream prunes stale entries | AutoDream consolidates findings |

A finding starts in tears as a note ("interesting result from experiment X"). It graduates to the wiki when it meets graduation criteria. The note stays in notes.toml (marked `graduated: true`) — still surfaces in code search — but its synthesis now lives in a structured page with cross-references.

---

## Appendix A: Industrial Documentation Wiki

The wiki architecture generalizes beyond code research to industrial documentation. Example: an AVEVA/Wonderware plant documentation wiki.

### Categories

```
~/plant-wiki/
  systems/          ← one page per control system (batch engine, historian, InTouch, System Platform)
  objects/          ← ArchestrA object types, templates, attributes, scripting patterns
  protocols/        ← OPC, DDE, SuiteLink, MQTT — connections, config, gotchas
  procedures/       ← operational procedures mapped to PLC routines and HMI screens
  alarms/           ← definitions, priorities, suppression logic, escalation paths
  tags/             ← naming conventions, cross-refs between PLC/historian/HMI tags
  troubleshooting/  ← known issues, root causes, fix procedures
```

### Source material

- AVEVA documentation PDFs → `cqs convert` → markdown (local, no API)
- InTouch/System Platform export files
- Historian configuration exports
- L5X/L5K PLC program exports (indexed by cqs with ST parser)
- Tribal knowledge captured during troubleshooting sessions

### Cross-reference graph

The value compounds at the intersections. "What happens when this alarm fires?" links to:
- The PLC routine that sets it (`systems/plc-controller-3.md` → L5X source)
- The HMI screen that displays it (`systems/intouch-line3.md`)
- The historian tag that logs it (`tags/alarm-tag-mapping.md`)
- The troubleshooting page for when it misfires (`troubleshooting/false-alarm-line3.md`)

That query currently requires asking three different people. With the wiki + cqs `gather`, it's one search that follows cross-references across all source types.

### No API calls required

The entire system runs locally:
- `cqs convert` for PDF→markdown (pymupdf4llm, local)
- `cqs index` for indexing all sources (local ONNX embeddings)
- `cqs gather` / `cqs search` for querying (local)
- Wiki maintenance via Claude Code session (the LLM reads sources and writes wiki pages)
- Optional: `--llm-summaries` for enrichment (Claude API, opt-in)

The wiki is markdown files in a git repo. The LLM that maintains it is the Claude Code session. No standing service, no API dependency for core functionality.

### Bootstrap from existing plant

1. Convert AVEVA PDFs to markdown (`cqs convert docs/aveva/*.pdf --output docs/aveva/`)
2. Index L5X/L5K files when available (`cqs index`)
3. Run `/wiki-bootstrap` — LLM reads converted docs + PLC source, creates stub pages
4. Run `/wiki-bootstrap` pass 2 — cross-references and synthesis
5. Ongoing: `/wiki-update` after each troubleshooting session or configuration change

### Relationship to PLC code search

The L5X/L5K parser feeds the wiki's `systems/` and `tags/` sections. A PLC routine name in the wiki links to its actual source code via cqs search. `cqs callers MyAOI --cross-project` (when implemented) traces calls across controllers. The wiki provides the "why" and "when"; the code provides the "how."
