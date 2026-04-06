# Wiki System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone, LLM-maintained research wiki with 4 skills (`/wiki-bootstrap`, `/wiki-update`, `/wiki-lint`, `/wiki-query`), a git-tracked directory structure, and bootstrap it from existing research artifacts.

**Architecture:** The wiki is a standalone git repo (`~/wiki/`) registered as a cqs reference project. Four Claude Code skills handle all writes. A commit-msg hook enforces the `wiki:` commit prefix convention. No Rust code — pure skills and markdown.

**Tech Stack:** Claude Code skills (SKILL.md), git, cqs ref, markdown, YAML frontmatter

**Spec:** `docs/superpowers/specs/2026-04-04-wiki-system-design.md`

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
- Create: `~/wiki/{experiments,techniques,models,datasets,findings,papers,evals}/.gitkeep`

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

Write `~/wiki/schema.md` with all page templates and conventions. Use 4-space indented blocks (not fenced code blocks) for the templates inside the file, since the file itself is markdown.

Templates to include: Experiment, Technique, Finding, Paper, Model, Dataset, Eval. Each template has the fields from the design spec (Status, Date, Confidence, etc.).

Conventions section covers: cross-reference format (`[name](../category/name.md)`), status/confidence vocabularies, commit message format, log entry format, YAML frontmatter fields, category management (suggest-category workflow), graduation criteria.

Full content is in the design spec under "Page Formats" and "schema.md" sections.

- [ ] **Step 6: Create `index.md`**

Write `~/wiki/index.md`:
```markdown
# Wiki Index

Last updated: 2026-04-06

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

## [2026-04-06] bootstrap | wiki infrastructure created
Empty scaffold. Skills installed. Awaiting bootstrap pass 1 for population.
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
git commit -m "wiki: bootstrap | initial scaffold"
```

The hook reads from `$1` (the COMMIT_EDITMSG file), and `git commit -m` does populate it. The `wiki:` prefix matches.

- [ ] **Step 10: Index with cqs and register as reference**

```bash
cd ~/wiki && cqs index
cd /mnt/c/Projects/cqs && cqs ref add wiki ~/wiki
```

- [ ] **Step 11: Verify search works**

```bash
cd /mnt/c/Projects/cqs && cqs "wiki schema conventions" --include-refs --json | head -5
```

Expected: results from `~/wiki/schema.md`.

---

### Task 2: Create `/wiki-update` skill

**Files:**
- Create: `/mnt/c/Projects/cqs/.claude/skills/wiki-update/SKILL.md`

- [ ] **Step 1: Write the skill**

The skill handles two workflows: ingest (new content) and graduation (notes → wiki).

**Ingest process:**
1. Read `$WIKI/schema.md` for the matching template
2. Check for existing page — update if exists, create if not
3. Write the page using the template, include YAML frontmatter
4. Check `$WIKI/findings/` for contradictions — update finding pages if found
5. Update cross-references (relative markdown links to/from related pages)
6. Update `index.md` (add entry, update count)
7. Append to `log.md`
8. Git commit with `wiki: ingest | <name>`
9. Reindex: `cd "$WIKI" && cqs index`

**Graduation process:**
1. Read `cqs notes list --json` for candidates meeting graduation criteria (sentiment + age, or contradiction)
2. For each candidate: create/update wiki page, update index + log
3. Git commit with `wiki: graduate | N notes from <project>`
4. Reindex

**Note:** The `--graduated` flag does not exist on `cqs notes update`. Track graduation in `log.md` instead — log which notes were graduated with their text prefix. The `/wiki-lint` skill can cross-check notes vs log to detect re-graduation.

**Arguments:** `experiment <name>`, `paper <name>`, `finding <claim>`, `technique <name>`, `model <name>`, `dataset <name>`, `eval <name>`, `--graduate`

**Wiki path:** `CQS_WIKI_PATH` env var, falls back to `~/wiki`.

- [ ] **Step 2: Commit**

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

Eight checks, grouped by severity:

**Fix now:** Broken links (grep for `](` targets, check file existence), index.md inconsistencies (directory count vs header count, missing/extra entries).

**Review:** Stale claims (findings referencing old experiments when newer ones exist), orphan pages (zero inbound links).

**Info:** Missing pages (link targets that don't exist), cross-reference gaps (experiments without finding links, findings without evidence links), category suggestions (count `suggest-category` log entries, surface at 5+).

**Maintenance:** Log rotation (>100 entries → move all but last 50 to `log-archive.md`).

After presenting findings, offer to fix automatically. Commit fixes with `wiki: lint | fixed N issues`. Reindex after.

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

Two-pass bootstrap from research artifacts.

**Pass 1 (stubs):** Read `~/training-data/research_log.md`, `~/training-data/RESULTS.md`, and `docs/notes.toml`. Identify all entities (experiments, models, datasets, techniques, papers, evals). Create stub pages with name, status, date, results table, one-line summary, empty cross-reference sections. Generate index.md. Git commit.

**Pass 2 (synthesis):** Read all stubs, add cross-references between pages, write finding claims with evidence citations and confidence levels, note contradictions, update index.md summaries. Verify all links resolve. Git commit. Reindex.

**Clean-room mode:** When source data reliability is uncertain, mark ALL numerical results as `**unverified**` in experiment pages. Qualitative content (techniques, findings, models) is reliable. After a clean-room eval re-runs all models, update with verified numbers via `/wiki-update`.

**Arguments:** `1` or `pass1`, `2` or `pass2`, `--source <path>` (default: `~/training-data`).

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

Synthesize an answer from wiki pages with citations.

**Process:** `cqs gather "<question>" --tokens 4000` to surface wiki pages, read relevant pages + one hop of cross-references, synthesize answer with citations. Optionally file back as a finding page (`--file`).

**Rules:** Always cite wiki pages not raw sources. Don't hallucinate — say "not answerable from wiki" if needed. Query-derived findings start at confidence "Speculative."

**Arguments:** `<question>`, `--file` (optional).

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

Add after existing skill entries:

```markdown
- `/wiki-bootstrap` -- populate wiki from research artifacts (two-pass)
- `/wiki-update` -- file experiments, papers, findings, techniques to wiki
- `/wiki-lint` -- wiki health check (broken links, orphans, stale claims)
- `/wiki-query` -- answer research questions from wiki with citations
```

- [ ] **Step 2: Add wiki section to CLAUDE.md**

Add near the Continuity section:

```markdown
## Wiki

Research wiki at `$CQS_WIKI_PATH` (default `~/wiki`). Registered as cqs reference: `cqs ref add wiki ~/wiki`.

- `/wiki-update` to file new content
- `/wiki-lint` for health checks
- `/wiki-query` to synthesize answers
- Wiki pages surface in `cqs gather` and `cqs search --include-refs`
```

- [ ] **Step 3: Add wiki skills to cqs-bootstrap portable list**

Read `.claude/skills/cqs-bootstrap/SKILL.md` and add wiki skills to the portable skills list.

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
cqs "wiki conventions schema" --include-refs --json | head -5
```

Expected: results from `~/wiki/schema.md`.

- [ ] **Step 4: Test `/wiki-update` with a specific finding**

Invoke `/wiki-update finding "E5-base architecture ceiling at 81% pipeline R@1"` and provide explicit content:

"Three experiments (margin sweep, band mining, iterative distillation) all null. E5-base + CG-filtered 200K + GIST 0.05 + 1 epoch is the ceiling. Confidence: High. Evidence: Exp 1, 2, 3 from SSD roadmap."

Verify:
- Finding page created at `~/wiki/findings/e5-base-ceiling.md`
- index.md updated with entry under Findings
- log.md has ingest entry
- Git commit with `wiki:` prefix

- [ ] **Step 5: Test `/wiki-lint`**

Invoke `/wiki-lint` and verify it:
- Reports the orphan finding page (no inbound links yet — expected for first page)
- No broken links
- Index counts match directory listing (1 finding)

---

### Task 8: Bootstrap wiki from research artifacts

**Files:**
- Modify: `~/wiki/` (many new pages)

This is where the wiki gets its value. Tasks 1-7 built infrastructure; this populates it.

- [ ] **Step 1: Run `/wiki-bootstrap pass1 --source ~/training-data`**

Creates stub pages for all entities found in:
- `~/training-data/research_log.md` (experiments 1-28)
- `~/training-data/RESULTS.md` (models, evals, baselines)
- `docs/notes.toml` (graduated notes)

Expected: ~40-60 stub pages across all categories.

Use clean-room mode for experiment numbers — mark results as `**unverified**` since we just re-baselined and some old numbers are stale. Qualitative content (what was tried, what technique was used) is reliable.

- [ ] **Step 2: Review stubs**

Spot-check 5-10 pages for correctness:
- Do experiment pages have the right configuration?
- Do model pages have the right parameter counts?
- Are techniques correctly classified as Production/Rejected?

- [ ] **Step 3: Run `/wiki-bootstrap pass2`**

Adds cross-references, writes finding claims, notes contradictions.

- [ ] **Step 4: Run `/wiki-lint`**

Catch orphans, broken links, missing cross-references from the bootstrap.

- [ ] **Step 5: Update verified numbers**

For models we re-baselined today (BGE-large 91.2%, BGE-large FT 91.9%, v9-200k 81.4%), update the experiment and model pages with verified numbers via `/wiki-update`.

- [ ] **Step 6: Final commit and reindex**

```bash
cd ~/wiki && git add -A && git commit -m "wiki: bootstrap | complete, N pages"
cd ~/wiki && cqs index
```
