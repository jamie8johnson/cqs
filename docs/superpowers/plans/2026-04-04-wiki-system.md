# Wiki System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone, LLM-maintained research wiki with 4 skills (`/wiki-bootstrap`, `/wiki-update`, `/wiki-lint`, `/wiki-query`) and a git-tracked directory structure.

**Architecture:** The wiki is a standalone git repo (`~/wiki/`) registered as a cqs reference project. Four Claude Code skills handle all writes. A pre-commit hook enforces the `wiki:` commit prefix convention. No Rust code — pure skills and markdown.

**Tech Stack:** Claude Code skills (SKILL.md), git, cqs ref, markdown, YAML frontmatter

---

## File Structure

**New files (wiki repo — `~/wiki/`):**
- `schema.md` — conventions, templates, vocabularies
- `index.md` — content catalog
- `log.md` — activity log
- `.cqsignore` — exclude log-archive.md from indexing
- `.gitignore` — minimal
- `.git/hooks/commit-msg` — enforce `wiki:` prefix

**New files (cqs project — `.claude/skills/`):**
- `.claude/skills/wiki-bootstrap/SKILL.md`
- `.claude/skills/wiki-update/SKILL.md`
- `.claude/skills/wiki-lint/SKILL.md`
- `.claude/skills/wiki-query/SKILL.md`

**Modified files:**
- `CLAUDE.md` — add wiki skills to skills list
- `.claude/skills/cqs-bootstrap/SKILL.md` — add wiki skills to portable list

---

### Task 1: Create the wiki git repo and scaffold

**Files:**
- Create: `~/wiki/schema.md`
- Create: `~/wiki/index.md`
- Create: `~/wiki/log.md`
- Create: `~/wiki/.cqsignore`
- Create: `~/wiki/.gitignore`
- Create: `~/wiki/.git/hooks/commit-msg`
- Create: `~/wiki/experiments/.gitkeep`
- Create: `~/wiki/techniques/.gitkeep`
- Create: `~/wiki/models/.gitkeep`
- Create: `~/wiki/datasets/.gitkeep`
- Create: `~/wiki/findings/.gitkeep`
- Create: `~/wiki/papers/.gitkeep`
- Create: `~/wiki/evals/.gitkeep`

- [ ] **Step 1: Create the wiki directory and init git**

```bash
mkdir -p ~/wiki
cd ~/wiki
git init
```

- [ ] **Step 2: Create `.gitignore`**

Write `~/wiki/.gitignore`:
```
.cqs/
```

- [ ] **Step 3: Create `.cqsignore`**

Write `~/wiki/.cqsignore`:
```
log-archive.md
```

- [ ] **Step 4: Create category directories**

```bash
cd ~/wiki
mkdir -p experiments techniques models datasets findings papers evals
touch experiments/.gitkeep techniques/.gitkeep models/.gitkeep datasets/.gitkeep findings/.gitkeep papers/.gitkeep evals/.gitkeep
```

- [ ] **Step 5: Create `schema.md`**

Write `~/wiki/schema.md`:
```markdown
# Wiki Schema

Conventions for maintaining this wiki. Read this before creating or updating pages.

## Page Templates

### Experiment

```
# <experiment-name>

**Status:** Production / Rejected / Superseded / In Progress
**Date:** YYYY-MM-DD
**Training time:** (if applicable)

## Configuration
- (key parameters)

## Results

| Eval | Metric | Value |
|------|--------|-------|
| ... | ... | ... |

## Key finding
(What this proved/disproved.)
See: [finding](../findings/finding.md), [technique](../techniques/technique.md)

## Contradictions / open questions
- ...
```

### Technique

```
# <technique-name>

**Status:** Production / Rejected / Experimental
**Novel:** Yes / No — (prior art)
**Implemented in:** `file.py`, `module.rs`

## What it does
(Concise description.)

## Why it matters
- (Evidence with links)

## Mechanism
(Implementation detail.)

## Evidence
See: [experiment](../experiments/experiment.md)

## Future
- ...
```

### Finding

```
# <finding — stated as a claim>

**Established:** YYYY-MM-DD
**Confidence:** High / Medium / Low / Speculative
**Implication:** (One-line practical consequence)

## The finding
(Claim, evidence, magnitude.)

## Caveats
- ...

## Contradictions
- [other](other.md): (how it conflicts)

## Related
[a](a.md), [b](b.md)
```

### Paper

```
# <paper-title>

**Authors:** ...
**Venue:** ... (year)
**Relevance:** (Why this matters)

## Thesis
(One paragraph.)

## Key findings
- (Bullets with links)

## Applicability
(What we tried, links to experiments)
```

### Model

```
# <model-name>

**Architecture:** ...
**Parameters:** ...
**Default dim:** ...

## Eval results

| Eval | R@1 | MRR | Notes |
|------|-----|-----|-------|
| ... | ... | ... | ... |

## Training
See: [experiment](../experiments/experiment.md)

## Notes
- ...
```

### Dataset

```
# <dataset-name>

**Size:** ...
**Source:** ...
**Used in:** [experiment](../experiments/experiment.md)

## Description
...

## Known issues
- ...
```

### Eval

```
# <eval-name>

**Query count:** ...
**Languages:** ...
**Type:** fixture / real-code / external

## Description
...

## Baseline results

| Model | R@1 | MRR |
|-------|-----|-----|
| ... | ... | ... |
```

## Conventions

- **Cross-references:** Relative markdown links only. `[name](../category/name.md)`.
- **Field names:** snake_case in frontmatter.
- **Status vocabulary:** Production / Rejected / Superseded / Planned / In Progress
- **Confidence vocabulary:** High / Medium / Low / Speculative
- **Commit messages:** `wiki: <verb> | <description>`. Verbs: bootstrap, ingest, graduate, lint, contradiction, query.
- **Log format:** `## [YYYY-MM-DD] verb | description` in log.md.
- **YAML frontmatter:** Optional on wiki pages. Fields: status, date, confidence, tags. Enables Obsidian Dataview queries.

## Graduation Criteria (notes.toml → wiki)

A note graduates when ANY of:
- Sentiment >= 0.5 or <= -0.5, AND age > 7 days
- Referenced in 3+ distinct search results within a session
- Manually tagged `wiki: true`
- Contradicts an existing wiki finding
```

- [ ] **Step 6: Create `index.md`**

Write `~/wiki/index.md`:
```markdown
# Wiki Index

Last updated: 2026-04-04

## Experiments (0)

## Techniques (0)

## Models (0)

## Datasets (0)

## Findings (0)

## Papers (0)

## Evals (0)
```

- [ ] **Step 7: Create `log.md`**

Write `~/wiki/log.md`:
```markdown
# Wiki Log

## [2026-04-04] bootstrap | wiki infrastructure created
Empty scaffold. Skills installed. Awaiting clean-room eval data for population.
```

- [ ] **Step 8: Install the commit-msg hook**

Write `~/wiki/.git/hooks/commit-msg`:
```bash
#!/bin/bash
MSG=$(cat "$1")
if ! echo "$MSG" | grep -q "^wiki:"; then
    echo "ERROR: Wiki commits must use 'wiki: <verb> | <description>' format."
    echo "Use /wiki-update or /wiki-lint to modify wiki pages."
    echo "Override: git commit --no-verify"
    exit 1
fi
```

```bash
chmod +x ~/wiki/.git/hooks/commit-msg
```

- [ ] **Step 9: Initial commit**

```bash
cd ~/wiki
git add -A
git commit --no-verify -m "wiki: bootstrap | initial scaffold"
```

(Use `--no-verify` for this one commit since the hook expects `wiki:` prefix and `git commit -m "wiki: bootstrap | ..."` should work, but the hook reads from COMMIT_EDITMSG which may not be populated with `-m`. Safer to skip for init.)

- [ ] **Step 10: Index with cqs and register as reference**

```bash
cd ~/wiki && cqs index
cd /mnt/c/Projects/cqs && cqs ref add wiki ~/wiki
```

- [ ] **Step 11: Verify search works**

```bash
cd /mnt/c/Projects/cqs && cqs "wiki schema conventions" --json | head -5
```

Expected: results from `~/wiki/schema.md`.

- [ ] **Step 12: Commit**

Already committed in step 9. Nothing to do.

---

### Task 2: Create `/wiki-update` skill

**Files:**
- Create: `/mnt/c/Projects/cqs/.claude/skills/wiki-update/SKILL.md`

- [ ] **Step 1: Write the skill**

Write `.claude/skills/wiki-update/SKILL.md`:
```markdown
---
name: wiki-update
description: File a new experiment, paper, finding, or technique to the wiki. Also handles notes.toml graduation.
disable-model-invocation: false
argument-hint: "<type> [--graduate]"
---

# Wiki Update

File new content to the wiki or graduate notes.

## Arguments

- `experiment <name>` — ingest a new experiment result
- `paper <name>` — ingest a new paper
- `finding <claim>` — file a new finding
- `technique <name>` — file a new technique
- `model <name>` — file a new model entry
- `dataset <name>` — file a new dataset entry
- `eval <name>` — file a new eval definition
- `--graduate` — check notes.toml for graduation candidates

## Wiki Path

Read from `CQS_WIKI_PATH` env var. Falls back to `~/wiki`.

```bash
WIKI="${CQS_WIKI_PATH:-$HOME/wiki}"
```

Verify the path exists and is a git repo before proceeding.

## Process (ingest)

1. **Read schema**: Read `$WIKI/schema.md` for the template matching the content type
2. **Check for existing page**: `ls $WIKI/<category>/<name>.md` — update if exists, create if not
3. **Read source material**: Ask the user what to ingest, or read from the current session context
4. **Write the page**: Use the template from schema.md. Include YAML frontmatter.
5. **Check for contradictions**: Read `$WIKI/findings/` pages. If the new content contradicts an existing finding, update the finding page with a Contradictions entry and note the contradiction in log.md.
6. **Update cross-references**: Add relative markdown links to/from related pages (experiments ↔ findings ↔ techniques).
7. **Update index.md**: Add entry under the correct category. Update the count in the section header.
8. **Append to log.md**: `## [YYYY-MM-DD] ingest | <name>`
9. **Git commit**:
```bash
cd "$WIKI" && git add -A && git commit -m "wiki: ingest | <name>"
```
10. **Reindex**: `cd "$WIKI" && cqs index`

## Process (graduation)

1. **Read notes**: `cqs notes list --json` in the current project
2. **Check graduation criteria** (from schema.md):
   - Sentiment >= 0.5 or <= -0.5, AND age > 7 days
   - Contradicts an existing wiki finding
3. **For each candidate**:
   - Determine category (finding, technique, or observation)
   - Create or update the wiki page
   - Mark the note as graduated: `cqs notes update "<text>" --graduated true`
   - Update index.md and log.md
4. **Git commit**: `cd "$WIKI" && git add -A && git commit -m "wiki: graduate | N notes from <project>"`
5. **Reindex**: `cd "$WIKI" && cqs index`

## Rules

- Always read schema.md first — templates may have evolved
- Never modify raw sources (research_log.md, RESULTS.md)
- Always check for contradictions before committing
- Use relative markdown links for all cross-references
- Commit message format: `wiki: ingest | <name>` or `wiki: graduate | <description>`
```

- [ ] **Step 2: Verify skill loads**

```bash
cd /mnt/c/Projects/cqs
# The skill should appear in Claude Code's skill list
```

- [ ] **Step 3: Commit**

```bash
cd /mnt/c/Projects/cqs
git add .claude/skills/wiki-update/SKILL.md
git commit -m "feat: add /wiki-update skill"
```

---

### Task 3: Create `/wiki-lint` skill

**Files:**
- Create: `/mnt/c/Projects/cqs/.claude/skills/wiki-lint/SKILL.md`

- [ ] **Step 1: Write the skill**

Write `.claude/skills/wiki-lint/SKILL.md`:
```markdown
---
name: wiki-lint
description: Health check the wiki — find stale claims, orphans, broken links, missing pages, inconsistent results.
disable-model-invocation: false
---

# Wiki Lint

Health check and maintenance pass on the wiki.

## Wiki Path

```bash
WIKI="${CQS_WIKI_PATH:-$HOME/wiki}"
```

## Checks

Run each check and report findings:

### 1. Broken links

```bash
grep -rn '](../' "$WIKI" --include="*.md" | while read line; do
    file=$(echo "$line" | cut -d: -f1)
    link=$(echo "$line" | grep -oP '\]\(\K[^)]+')
    dir=$(dirname "$file")
    target="$dir/$link"
    [ ! -f "$target" ] && echo "BROKEN: $file -> $link"
done
```

### 2. Orphan pages

Pages with zero inbound links (not referenced by any other page or index.md):

```bash
for f in "$WIKI"/*/*.md; do
    name=$(basename "$f")
    refs=$(grep -rl "$name" "$WIKI" --include="*.md" | grep -v "$f" | wc -l)
    [ "$refs" -eq 0 ] && echo "ORPHAN: $f"
done
```

### 3. Missing pages

Concepts referenced in links but with no corresponding file. Extract all link targets and check existence.

### 4. Stale claims

Read each finding page. If the finding references experiments, check if newer experiments in the same category exist that might contradict. Flag for human review — don't auto-update.

### 5. Index.md consistency

- Count pages per category directory. Compare with count in index.md header.
- Check that every page has an entry in index.md.
- Check that every index.md entry points to an existing page.

### 6. Log rotation

```bash
ENTRIES=$(grep -c "^## \[" "$WIKI/log.md")
if [ "$ENTRIES" -gt 100 ]; then
    # Move all but last 50 entries to log-archive.md
fi
```

### 7. Cross-reference completeness

For each experiment page, check that it links to at least one finding or technique. For each finding, check that it links to at least one experiment as evidence.

## Output

Report findings grouped by severity:
- **Fix now**: Broken links, index inconsistencies
- **Review**: Stale claims, orphan pages
- **Info**: Missing pages, cross-reference gaps

## Fix mode

After presenting findings, offer to fix automatically:
- Broken links → remove or prompt for correction
- Index inconsistencies → rebuild index.md from directory listing
- Log rotation → move entries to archive
- Orphans → add to index.md or prompt for deletion

## Commit

```bash
cd "$WIKI" && git add -A && git commit -m "wiki: lint | fixed N issues"
cd "$WIKI" && cqs index
```
```

- [ ] **Step 2: Commit**

```bash
cd /mnt/c/Projects/cqs
git add .claude/skills/wiki-lint/SKILL.md
git commit -m "feat: add /wiki-lint skill"
```

---

### Task 4: Create `/wiki-bootstrap` skill

**Files:**
- Create: `/mnt/c/Projects/cqs/.claude/skills/wiki-bootstrap/SKILL.md`

- [ ] **Step 1: Write the skill**

Write `.claude/skills/wiki-bootstrap/SKILL.md`:
```markdown
---
name: wiki-bootstrap
description: Two-pass bootstrap of the wiki from research artifacts. Pass 1 creates stubs, Pass 2 adds cross-references and synthesis.
disable-model-invocation: false
argument-hint: "<pass> [--source <path>]"
---

# Wiki Bootstrap

Populate the wiki from research artifacts.

## Arguments

- `1` or `pass1` — create stub pages (mechanical extraction)
- `2` or `pass2` — add cross-references and synthesis
- `--source <path>` — path to source material (default: `~/training-data`)

## Wiki Path

```bash
WIKI="${CQS_WIKI_PATH:-$HOME/wiki}"
```

## Prerequisites

- Wiki scaffold must exist (`$WIKI/schema.md`, `$WIKI/index.md`)
- Source material must be accessible

## Pass 1: Stubs

1. **Read source material**:
   - `<source>/research_log.md` — experiment timeline
   - `<source>/RESULTS.md` — eval numbers
   - Current project's `docs/notes.toml` — observations

2. **Identify entities**: Scan sources for:
   - Experiments (each `### Exp N` section in research log)
   - Models (each model evaluated — E5-base, BGE-large, v9-200k, etc.)
   - Datasets (CSN, 200K balanced, fixture queries, real queries)
   - Techniques (enrichment, CG filtering, GIST loss, Matryoshka, etc.)
   - Papers (SSD, any cited papers)
   - Evals (fixture 296q, real 50q, CoIR)

3. **Create stub pages**: For each entity, create a page using the template from `$WIKI/schema.md`. Include:
   - Name, status, date
   - Results table with numbers (mark unverified numbers with `*`)
   - One-line summary
   - Empty cross-reference sections (filled in pass 2)

4. **Generate index.md**: Rebuild from directory listing.

5. **Append to log.md**: `## [YYYY-MM-DD] bootstrap | pass 1, N stubs created`

6. **Git commit**: `cd "$WIKI" && git add -A && git commit -m "wiki: bootstrap | pass 1, N stubs"`

## Pass 2: Synthesis

1. **Read all stub pages**: Build a mental map of entities and relationships.

2. **Add cross-references**: For each page:
   - Link experiments → findings they established
   - Link experiments → techniques they tested
   - Link findings → experiments that provide evidence
   - Link techniques → experiments and findings
   - Link models → experiments that evaluated them

3. **Write finding claims**: For each identified insight, write a clear claim with evidence citations and confidence level.

4. **Note contradictions**: Where experiments disagree, add Contradictions sections to the relevant finding pages.

5. **Update index.md summaries**: Replace stub one-liners with meaningful summaries.

6. **Verify links**: `grep -rn '](../' "$WIKI" --include="*.md"` — check all resolve.

7. **Append to log.md**: `## [YYYY-MM-DD] bootstrap | pass 2, cross-references and synthesis`

8. **Git commit**: `cd "$WIKI" && git add -A && git commit -m "wiki: bootstrap | pass 2, cross-references and synthesis"`

9. **Reindex**: `cd "$WIKI" && cqs index`

## Clean-Room Mode

When source data reliability is uncertain, use clean-room mode:

1. Create stubs for techniques, findings, models, and evals (qualitative content is reliable)
2. Mark ALL numerical results as `**unverified**` in experiment pages
3. After a clean-room eval session re-runs all models, update experiment pages with verified numbers via `/wiki-update`

This separates "what we tried" (reliable) from "what numbers we got" (needs re-verification).

## Rules

- Never modify source material
- Mark unverified numbers with `*` or `**unverified**`
- Each pass ends with a git commit
- Run `/wiki-lint` after pass 2 to catch orphans and broken links
```

- [ ] **Step 2: Commit**

```bash
cd /mnt/c/Projects/cqs
git add .claude/skills/wiki-bootstrap/SKILL.md
git commit -m "feat: add /wiki-bootstrap skill"
```

---

### Task 5: Create `/wiki-query` skill

**Files:**
- Create: `/mnt/c/Projects/cqs/.claude/skills/wiki-query/SKILL.md`

- [ ] **Step 1: Write the skill**

Write `.claude/skills/wiki-query/SKILL.md`:
```markdown
---
name: wiki-query
description: Synthesize an answer from wiki pages with citations. Optionally file the answer back as a new finding.
disable-model-invocation: false
argument-hint: "<question> [--file]"
---

# Wiki Query

Answer a research question using wiki pages as sources.

## Arguments

- `<question>` — the research question to answer
- `--file` — file the answer back as a new finding page

## Wiki Path

```bash
WIKI="${CQS_WIKI_PATH:-$HOME/wiki}"
```

## Process

1. **Search**: `cqs gather "<question>" --tokens 4000` — surfaces wiki pages and code together via the reference index.

2. **Read relevant pages**: For each wiki result, read the full page. Follow cross-references one hop deep to gather supporting evidence.

3. **Synthesize**: Answer the question with citations to specific wiki pages. Format:
   ```
   [Answer text]

   Sources:
   - [page-name](path/to/page.md) — what it contributed
   - [page-name](path/to/page.md) — what it contributed
   ```

4. **File back** (if `--file`):
   - Create a finding page in `$WIKI/findings/` with the synthesized answer
   - Add cross-references to source pages
   - Update index.md and log.md
   - `cd "$WIKI" && git add -A && git commit -m "wiki: query | <question summary>"`
   - `cd "$WIKI" && cqs index`

## Rules

- Always cite wiki pages, not raw sources
- If a question can't be answered from existing wiki pages, say so — don't hallucinate
- If filing back, use the finding template from schema.md
- Confidence level for query-derived findings: start at "Speculative" unless strongly supported
```

- [ ] **Step 2: Commit**

```bash
cd /mnt/c/Projects/cqs
git add .claude/skills/wiki-query/SKILL.md
git commit -m "feat: add /wiki-query skill"
```

---

### Task 6: Update CLAUDE.md and bootstrap skill

**Files:**
- Modify: `/mnt/c/Projects/cqs/CLAUDE.md`
- Modify: `/mnt/c/Projects/cqs/.claude/skills/cqs-bootstrap/SKILL.md`

- [ ] **Step 1: Add wiki skills to CLAUDE.md skills list**

In `CLAUDE.md`, find the skills list section and add after the existing entries:

```markdown
- `/wiki-bootstrap` -- populate wiki from research artifacts (two-pass)
- `/wiki-update` -- file experiments, papers, findings, techniques to wiki
- `/wiki-lint` -- wiki health check (broken links, orphans, stale claims)
- `/wiki-query` -- answer research questions from wiki with citations
```

- [ ] **Step 2: Add wiki env var convention to CLAUDE.md**

In `CLAUDE.md`, add near the Continuity section:

```markdown
## Wiki

Research wiki at `$CQS_WIKI_PATH` (default `~/wiki`). Registered as cqs reference: `cqs ref add wiki ~/wiki`.

- `/wiki-update` to file new content
- `/wiki-lint` for health checks
- `/wiki-query` to synthesize answers
- Wiki pages surface in `cqs gather` and `cqs search` via reference index
```

- [ ] **Step 3: Add wiki skills to cqs-bootstrap portable list**

Read `.claude/skills/cqs-bootstrap/SKILL.md` and add wiki skills to the portable skills list so new projects get them.

- [ ] **Step 4: Commit**

```bash
cd /mnt/c/Projects/cqs
git add CLAUDE.md .claude/skills/cqs-bootstrap/SKILL.md
git commit -m "docs: add wiki skills to CLAUDE.md and bootstrap"
```

---

### Task 7: Set CQS_WIKI_PATH and verify end-to-end

**Files:**
- Modify: `~/.bashrc`

- [ ] **Step 1: Add CQS_WIKI_PATH to bashrc**

Add above the interactive guard in `~/.bashrc`:

```bash
export CQS_WIKI_PATH="$HOME/wiki"
```

- [ ] **Step 2: Source it**

```bash
source ~/.bashrc
```

- [ ] **Step 3: Verify the wiki is searchable**

```bash
cd /mnt/c/Projects/cqs
cqs "wiki conventions schema" --json | head -5
```

Expected: results from `~/wiki/schema.md` with `source: "wiki"`.

- [ ] **Step 4: Test `/wiki-update` with a manual finding**

Invoke `/wiki-update finding "enrichment stack compresses model differences"` and verify:
- Finding page created at `~/wiki/findings/enrichment-compresses-model-differences.md`
- index.md updated with entry
- log.md has ingest entry
- Git commit with `wiki:` prefix

- [ ] **Step 5: Test `/wiki-lint`**

Invoke `/wiki-lint` and verify it:
- Reports the orphan finding page (no inbound links yet — expected for first page)
- No broken links
- Index counts match directory listing

- [ ] **Step 6: Commit bashrc change**

The bashrc change is outside the repo — no git commit needed.

---

## Post-Plan: Clean-Room Eval Session

Not part of this plan. After the wiki infrastructure is built:

1. Re-run all model evals (E5-base, BGE-large, v9-200k, BGE-large-FT) on current code
2. Record verified numbers in RESULTS.md
3. Run `/wiki-bootstrap pass1 --source ~/training-data` in clean-room mode
4. Run `/wiki-bootstrap pass2` for cross-references
5. Run `/wiki-lint` to verify health
