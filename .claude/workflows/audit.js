export const meta = {
  name: 'audit',
  description: '16-category code-audit fan-out (one parallel stage) + a synthesize stage that triages and writes the findings/triage files',
  whenToUse:
    'The discovery + synthesis half of the /audit skill. Runs all 16 audit categories as independent auditors in a single parallel stage, then a synthesize agent dedups/clusters, buckets into P1-P4 (by the difficulty x impact the auditors return), and WRITES docs/audit-findings.md + docs/audit-triage.md. The orchestrator still reviews the triage (the cornerstone P1-vs-P2 call) and owns fix-prompt generation/review + the serialized landing. Optional args.churn = a string of recently-changed areas to scrutinize.',
  phases: [{ title: 'Audit' }, { title: 'Synthesize' }],
}

// Auditors run on the review/judge model. Per the 2026-06-12 fable US-export
// disable that is opus for now — flip AUDITOR_MODEL back to 'fable' when it is
// restored. Security is ALWAYS opus: fable's cyber classifiers risk a mid-run
// refusal that would kill that category's coverage for the whole audit.
const AUDITOR_MODEL = 'opus'

const FINDINGS_SCHEMA = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          title: { type: 'string', description: 'Short finding title' },
          difficulty: { type: 'string', enum: ['easy', 'medium', 'hard'] },
          impact: { type: 'string', enum: ['high', 'medium', 'low'] },
          location: { type: 'string', description: 'file:line(s) or symbol' },
          description: { type: 'string', description: 'What is wrong + why it matters; cite the code you read' },
          suggested_fix: { type: 'string' },
          existing_issue: { type: 'string', description: 'open issue # if this overlaps one, else empty' },
        },
        required: ['title', 'difficulty', 'impact', 'location', 'description', 'suggested_fix'],
      },
    },
    cleared: { type: 'string', description: 'Brief note on what was examined and found clean (the adversarial refutation)' },
  },
  required: ['findings', 'cleared'],
}

// The 16 categories (the two former batches, merged into one stage):
// [name, scope, mandatory-first-step].
const CATEGORIES = [
  ['Code Quality', 'Dead code, duplication, complexity, coupling, cohesion, module boundaries, convenience wrappers that hardcode defaults (a fn calling foo_with_x(HARDCODED) that masks miswiring when the default changes).', 'Run `cqs dead --json` and `cqs health --json`; grep for convenience wrappers hardcoding dims/defaults.'],
  ['Documentation', 'Accuracy, completeness, staleness of docs AND code comments vs the actual code (esp. comments describing behavior the code no longer has).', 'Run `cqs health --json` for staleness counts; spot-check high-traffic module doc comments.'],
  ['API Design', 'Consistency, ergonomics, naming, type design across the command-core surface (*_core / *Args / *Output, CLI cmd_* vs daemon dispatch_*).', 'Grep the command-core pattern; check CLI==daemon parity surface.'],
  ['Error Handling', 'Result chains, error context, recovery, swallowed errors (.unwrap_or_default() hiding store errors, bare ? losing context, errors logged-and-dropped).', 'Grep for unwrap_or_default, `let _ =`, `.ok()` on Results, swallowed match arms.'],
  ['Observability', 'Logging/tracing coverage, debuggability — new public fns missing tracing spans, error paths with no warn!, silent fallbacks.', 'Grep `tracing::` patterns; check the newest subsystems for span coverage.'],
  ['Test Coverage (adversarial)', 'Edge/sad-path gaps: malformed input, NaN/Inf embeddings, concurrent access, empty queries, huge/unicode inputs, error paths not tested. Functions taking user input or external data should have adversarial tests.', 'Run `cqs health --json` (untested hotspots); find user-input/embedding/parse fns lacking malformed-input tests.'],
  ['Robustness', 'unwrap/expect/panic paths in non-test code, edge cases (empty/huge/unicode/malformed), arithmetic overflow, indexing panics.', 'Grep `.unwrap()`, `.expect(`, `panic!`, `[0]`, `as usize` truncation in non-test code.'],
  ['Scaling & Hardcoded Limits', 'Constants that should scale with model config / corpus size / hardware; magic numbers without rationale; caps/clamps/capacities/Durations as literals.', 'Grep const definitions, `.clamp(`, capacity/Duration literals, dim literals.'],
  ['Algorithm Correctness', 'Off-by-one, boundary conditions, logic errors — esp. ranking/fusion, graph add/subtract, BFS depth, dedup, id/offset computation.', 'Use `cqs explain <fn> --json` on the algorithmic functions.'],
  ['Extensibility', 'Adding a feature/language/preset/command without surgery; hardcoded values that block extension; missing trait seams.', 'Run `cqs health --json`; check registration paths for hardcoded enumerations.'],
  ['Platform Behavior', 'OS differences, path handling, WSL quirks, #[cfg(target_os)]/#[cfg(unix)] divergence (the class that broke v1.46.0 darwin — check for OTHER shapes beyond the one guarded by tests/platform_cfg_sweep_test.rs), /proc, openat2, file-locking.', 'Grep cfg(target_os/unix/windows/not), std::os::, /proc, path canonicalization; reason about macOS/Windows expansion.'],
  ['Security', 'Injection, path traversal, file permissions, secrets, access control — overlay-root validation, daemon socket (same-uid 0o600), FTS/notes injection, indexed-content trust boundary, serve auth.', 'Read SECURITY.md threat model; examine validation, socket perms, and input paths.'],
  ['Data Safety', 'Corruption, validation, migrations, races, deadlocks, thread safety — schema/PARSER_VERSION migration read paths, side-table writes, daemon epoch/cache invalidation, WAL, concurrent index+query.', 'Examine the migration chain, daemon caches/epochs, side-table crud, WAL/transaction boundaries.'],
  ['Performance', 'O(n^2), unnecessary iterations, missing batching/caching, I/O patterns, redundant allocations/clones in hot paths (search, embed, index, overlay build).', 'Run `cqs health --json` (hotspots); scrutinize hot loops for quadratic or per-element clone patterns.'],
  ['Resource Management', 'Memory usage, startup time, idle cost, OOM protection, leaks — unbounded caches/Vecs, fd lifecycle, model/session lifetimes, daemon idle footprint.', 'Grep unbounded collections, fd/OwnedFd handling, cache eviction (or lack), large buffers without bounds.'],
  ['Test Coverage (happy path)', 'Missing tests for high-caller public functions, untested modules, integration-test gaps, weak/meaningless assertions (assert!(true)-shaped, assert-no-panic but not the result).', 'Run `cqs health --json` (untested hotspots); find high-caller public fns with no direct test + assertions that do not bite.'],
]

const churn = (typeof args === 'object' && args && args.churn) ? String(args.churn) : 'Scrutinize the most recently-changed subsystems hardest (check `git log --oneline -30` and the newest modules).'

const COMMON = `Repo: cqs (Rust semantic code-search, cwd is the repo root). AUDIT MODE IS ON — examine code DIRECTLY, do not trust notes; cqs excludes notes while audit-mode is on. Be skeptical and concrete: every finding must cite the actual code at a real file:line you READ, not a guess.

Dedup discipline:
- Read the most recent archived triage (the highest-version \`docs/audit-triage-v*.md\` — \`ls\` to find it) and skim older titles — SKIP anything already triaged/fixed there.
- Cross-check against open issues (\`gh issue list\` via PowerShell if needed). If a finding overlaps one, set existing_issue and only report if there is something NEW.

Churn focus: ${churn}

Return ALL findings via the StructuredOutput tool. Do NOT write or append to ANY file — the orchestrator aggregates. If a category is genuinely mined out, return few/zero findings + a 'cleared' note; do not pad. Quality over count.`

function auditPrompt(cat, scope, mandatory) {
  return `You are the **${cat}** auditor.\n\nScope: ${scope}\n\nMandatory first step: ${mandatory}\n\n${COMMON}`
}

phase('Audit')
// One stage of 16: hand all categories to a single parallel() and let the
// runtime's concurrency cap (min(16, cores-2)) schedule them — no artificial
// barrier stalling a second batch behind the slowest of the first.
const results = await parallel(
  CATEGORIES.map(([cat, scope, mand]) => () =>
    agent(auditPrompt(cat, scope, mand), {
      agentType: 'auditor',
      model: cat === 'Security' ? 'opus' : AUDITOR_MODEL,
      label: `audit:${cat}`,
      phase: 'Audit',
      schema: FINDINGS_SCHEMA,
    }).then((r) => ({ cat, ...r }))
  )
).then((rs) => rs.filter(Boolean))

const findings = results.flatMap((r) => (r.findings || []).map((f) => ({ category: r.cat, ...f })))
const byCategory = results.map((r) => ({ category: r.cat, count: r.findings?.length || 0, cleared: r.cleared }))
const total = findings.length
log(`audit discovery: ${total} findings across ${results.length} categories`)

// Deterministic first-pass triage from the (difficulty x impact) the auditors
// already return. P1 easy+high / P2 medium+high / P3 easy+(low|med) / P4 the
// rest (hard, low-impact non-easy, or already tracked by an open issue). The
// synthesizer refines these — they are a starting point, not the verdict.
function proposeTier(f) {
  if ((f.existing_issue || '').trim()) return 'P4'
  const hi = f.impact === 'high', easy = f.difficulty === 'easy', hard = f.difficulty === 'hard'
  if (easy && hi) return 'P1'
  if (hi && !hard) return 'P2'
  if (easy) return 'P3'
  return 'P4'
}
const proposed = findings.map((f) => ({ ...f, proposed_tier: proposeTier(f) }))

phase('Synthesize')
// A workflow SCRIPT has no filesystem access, but an AGENT does — so the scribe
// stage writes the two artifacts. It also dedups/clusters, refines the tiers
// (the reduce that needs the whole set in one mind), AND assigns each row a
// disposition route the next stage executes.
const TRIAGE_SCHEMA = {
  type: 'object',
  properties: {
    summary: { type: 'string', description: 'Tight prose: per-tier counts, P1/P2 titles+IDs, dedup merges, files-written confirmation' },
    rows: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          category: { type: 'string' },
          title: { type: 'string' },
          location: { type: 'string' },
          tier: { type: 'string', enum: ['P1', 'P2', 'P3', 'P4'] },
          route: { type: 'string', enum: ['auto-fix', 'issue', 'tracked', 'inline', 'drop'] },
          fix_approach: { type: 'string' },
        },
        required: ['id', 'title', 'location', 'tier', 'route', 'fix_approach'],
      },
    },
  },
  required: ['summary', 'rows'],
}
const synthPrompt = `You are the audit synthesizer for the cqs repo (cwd is the repo root). The 16-category discovery fan-out produced these ${total} findings (each with a deterministic proposed_tier you should REFINE, not blindly accept):

${JSON.stringify(proposed)}

Do, in order:
1. Stamp the version (from Cargo.toml \`version\`) and today's date (\`date +%Y-%m-%d\`).
2. DEDUP / CLUSTER: merge same-root-cause findings across categories into one row (note the merge); drop any that re-state an already-fixed item in the latest archived triage \`docs/audit-triage-v1.42.0.md\`.
3. Assign stable IDs: \`<CAT>-V<version>-<n>\` (e.g. RM-V1.46.1-1). Pick a short category prefix.
4. REFINE tiers with judgment (P1 easy+high → fix now; P2 medium+high → fix in batch; P3 easy+low/med → fix if time; P4 hard/low/already-tracked → issue or inline). Note when you override a proposed_tier and why. Findings with existing_issue stay P4 (tracked) unless there's a NEW slice.
5. Assign each row a DISPOSITION route + fix_approach: \`auto-fix\` (the fix is mechanical AND verifiable — doc corrections, added tests, one-line warns, dead-code deletion, simple renames), \`issue\` (needs a human/judgment call — architecture, API shape, perf tradeoff, or hard), \`tracked\` (already an open GitHub issue — name it in fix_approach), \`inline\` (a trivial rider that lands with another row), or \`drop\` (not a real finding). ROUTE BY FIX-NATURE, NOT TIER: a high-impact finding whose fix is a judgment call (e.g. a panic-policy or API decision) is \`issue\`, never \`auto-fix\`, even at P1/P2.
6. WRITE \`docs/audit-findings.md\`: a \`# Audit Findings — v<version>\` header, then \`## <Category>\` sections, each finding as \`#### <title>\` + Difficulty/Impact, Location, Description, Suggested fix (bullets). Overwrite the file.
7. WRITE \`docs/audit-triage.md\`: \`# Audit Triage (v<version>)\` + date, a Summary-by-priority count table, then \`## P1\`/\`## P2\`/\`## P3\`/\`## P4\` tables with columns \`| ID | Finding | Location | Route | Status |\` (Status starts "open"). Then a \`## Carried forward from v1.42.0\` section: read \`docs/audit-triage-v1.42.0.md\`'s still-open items, spot-grep the ambiguous ones against current main, list the survivors as CF-P2/CF-P3 — and explicitly flag that full carry-forward reconciliation against all PRs since v1.42.0 is the orchestrator's to confirm.
8. Use \`cat > file <<'EOF' ... EOF\` (heredoc) or the Write tool to write the files. Do NOT touch any source code.

Return via StructuredOutput: a tight \`summary\` (per-tier counts, P1+P2 titles+IDs, dedup merges, files-written confirmation) AND the \`rows\` array (one per triaged row: id, category, title, location, tier, route, fix_approach).`

const synthesis = await agent(synthPrompt, { model: 'opus', label: 'synthesize', phase: 'Synthesize', schema: TRIAGE_SCHEMA })
const rows = synthesis.rows || []

// ---- Disposition: prepare reversible artifacts per row. The IRREVERSIBLE acts
// (push/PR/merge to protected main, `gh issue create`) stay in the main loop —
// the workflow only produces verified fix-prompts, optional throwaway fix
// branches, and issue DRAFTS, all of which the orchestrator gates before acting.
phase('Disposition')
const FIXPROMPT_SCHEMA = {
  type: 'object',
  properties: {
    files: { type: 'array', items: { type: 'string' } },
    current_code: { type: 'string', description: 'verbatim current code to be replaced (for line-drift matching)' },
    replacement_code: { type: 'string' },
    why: { type: 'string' },
    blocked: { type: 'string', description: "non-empty if the fix turned out to need judgment / didn't apply cleanly on reading source" },
  },
  required: ['files', 'current_code', 'replacement_code', 'why'],
}
const VERDICT_SCHEMA = {
  type: 'object',
  properties: { verdict: { type: 'string', enum: ['VERIFIED', 'NEEDS-FIX'] }, notes: { type: 'string' } },
  required: ['verdict', 'notes'],
}
const ISSUE_SCHEMA = {
  type: 'object',
  properties: { title: { type: 'string' }, body: { type: 'string', description: 'markdown: Problem / Location / Impact / Options-for-the-human' } },
  required: ['title', 'body'],
}
const wantImplement = !!(args && args.implement)
const fixRows = rows.filter((r) => r.route === 'auto-fix')
const issueRows = rows.filter((r) => r.route === 'issue')
log(`disposition: ${fixRows.length} auto-fix, ${issueRows.length} issue${wantImplement ? ' (implement ON)' : ' (prepare-only)'}`)

async function disposeFix(r) {
  // gen + verify run on the read-only `Explore` agent type (Read/Grep/Bash, NO
  // Edit/Write) so prepare-mode CANNOT mutate the working tree — they produce
  // the change as DATA only. The actual write happens later, either in the
  // gated main-loop implementer or (args.implement) the worktree-isolated
  // lane-implementer below. (Prompt-only "don't apply" is not enough — agents
  // over-apply a "ready-to-apply fix"; tool-restriction is the real guard.)
  const gen = await agent(
    `Produce a fix SPECIFICATION (data only — do NOT modify, create, or write any file) for audit finding ${r.id}: "${r.title}" at ${r.location}. Intended change: ${r.fix_approach}. READ the real source at the cited lines first. Return the file path(s), the exact CURRENT code verbatim (so it can be matched), the REPLACEMENT code, and a one-line why. If on reading the source the fix actually needs a judgment call or doesn't apply cleanly, set 'blocked' with the reason instead of forcing it.`,
    { agentType: 'Explore', label: `fix-gen:${r.id}`, phase: 'Disposition', schema: FIXPROMPT_SCHEMA }
  )
  const ver = await agent(
    `Adversarially verify this proposed fix against the REAL current source (read the file — do NOT modify anything). Check: does current_code match the file verbatim (line drift)? does the replacement compile — types, imports, API existence? any edge case missed? Default to NEEDS-FIX if unsure.\n\n${JSON.stringify(gen)}`,
    { agentType: 'Explore', label: `fix-verify:${r.id}`, phase: 'Disposition', schema: VERDICT_SCHEMA }
  )
  let implemented = null
  if (wantImplement && ver.verdict === 'VERIFIED' && !(gen.blocked || '').trim()) {
    implemented = await agent(
      `Apply this verified audit fix on a new branch \`fix-audit-${r.id}\` (commit, do NOT push; run the targeted tests for the touched code + clippy the touched crate). Report the branch name, whether tests/clippy passed, and any deviation from the prompt:\n${JSON.stringify(gen)}`,
      { agentType: 'lane-implementer', model: 'opus', isolation: 'worktree', label: `fix-impl:${r.id}`, phase: 'Disposition' }
    )
  }
  return { id: r.id, tier: r.tier, kind: 'fix', verdict: ver.verdict, verify_notes: ver.notes, blocked: gen.blocked || '', prompt: gen, implemented }
}

async function disposeIssue(r) {
  const draft = await agent(
    `Draft a GitHub issue for audit finding ${r.id}: "${r.title}" at ${r.location}. It is NOT auto-fixable because: ${r.fix_approach}. Return a concise title and a markdown body (Problem / Location / Impact / Options or considerations for the human deciding). Read source as needed but do NOT modify any file, and do NOT post the issue.`,
    { agentType: 'Explore', label: `issue-draft:${r.id}`, phase: 'Disposition', schema: ISSUE_SCHEMA }
  )
  return { id: r.id, tier: r.tier, kind: 'issue', title: draft.title, body: draft.body }
}

const dispositions = (await parallel([
  ...fixRows.map((r) => () => disposeFix(r)),
  ...issueRows.map((r) => () => disposeIssue(r)),
])).filter(Boolean)

const verified = dispositions.filter((d) => d.kind === 'fix' && d.verdict === 'VERIFIED' && !d.blocked).length
log(`disposition done: ${verified}/${fixRows.length} fixes verified, ${issueRows.length} issue drafts${wantImplement ? `, ${dispositions.filter((d) => d.implemented).length} branches` : ''}`)

return {
  total,
  byCategory,
  findings,
  triagePath: 'docs/audit-triage.md',
  findingsPath: 'docs/audit-findings.md',
  summary: synthesis.summary,
  rows,
  dispositions,
}
