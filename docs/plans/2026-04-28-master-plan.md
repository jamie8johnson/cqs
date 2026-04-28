# Master Plan (snapshot 2026-04-28)

State after the indirect-injection cluster (#1166-#1170) landed on main, ahead of the v1.30.1 tag. Captures the strategic posture, near-term roadmap, and the publishing lever that matters more than any single feature.

## Strategic posture

1. **Agent-first is the only durable differentiator** — and it's fleeting. Cursor / Sourcegraph / Copilot will pivot to be more agent-friendly as agents become more important consumers. They have more resources, more users, more reach. Specific technical moves (`trust_level`, freshness contracts, JSON-first output) are retrofittable in a quarter or two.
2. **What might not be fleeting:**
   - **Vocabulary.** Whoever names the category gets recall. Sourcegraph is "code search" because they named it; grep predated them by decades. If cqs names "agent-first code intelligence" / "between-turns consistency" / "freshness contract" first and clearly, people reach for those words.
   - **Being agent-developed.** cqs is built BY agents, FOR agents. Design tightness from eating your own dogfood while the dogfood is also the dogeater. Hard to fake without reorganizing dev process.
   - **Constraints hard to retrofit.** Sourcegraph can't easily abandon server-side; Cursor can't easily decouple from VSCode. cqs is local-first-CLI-no-GUI by design — the right shape for agent harnesses (Claude Code, Codex CLI, agent SDKs) that don't have a JetBrains plugin.
3. **Strategic implication: prioritize publishing the framing over capability ladder-climbing.** The features ship via the issues we've filed; they will be copied; that's expected. The vocabulary articulated cleanly while the gap is still visible is what compounds.

## Design discipline (load-bearing)

- **Defense in depth, layered not stacked.** Multiple cheap mitigations that compose; each is fine if the others fail. (Indirect-injection cluster: 5 mitigations across 4 PRs, none sufficient alone.)
- **Demote, don't remove.** Preserve capability behind louder gates rather than killing capability outright. `--improve-docs --apply` still exists; the safe mode is just the default. Pattern: safe default + loud opt-in for dangerous mode.
- **Flag length matches risk tier.** Tier 4 (writes to project tree) → `--apply`. Tier 5+ (broader scope) → uglier flags so they're hard to type by accident. The flag length is a design choice, not just naming.
- **Cost framing vs duals framing.** Cost frame surfaces removal candidates, aggregation pressure, shared substrate, simplification-over-labeling. Dual frame surfaces what you'd lose by removing a feature. Switch deliberately; pick one and stick is how you over-mitigate or under-remove.
- **Every LLM-in-the-loop stage adds an injection surface.** Pure indexing doesn't. Surface count grows with LLM-stage count, not feature count. Plan mitigations per-stage, not per-feature.

## Immediate (in flight / unreleased on main)

- **v1.30.1 release** — indirect-injection cluster + earlier post-1.30.0 work. Pending: tag, crates.io publish, GitHub release. Local binary refresh after each cluster merge already happens; release ceremony should follow the same pattern but at version-cut cadence.
- **Tears + roadmap commit** — PROJECT_CONTINUITY.md, ROADMAP.md, docs/notes.toml dirty on main; commit alongside the close-issues cleanup PR (or as a chore commit).

## Near-term roadmap (next ~2 weeks)

Ordered by leverage, not effort:

1. **#1182 — perfect watch mode (3-layer reconciliation).** Closes the largest visible gap between cqs and similar tools: missed-event classes (bulk git ops, WSL 9P, external writes). Three layers: git hooks + periodic reconciliation + `cqs status --watch-fresh --wait` API. **Positioning lever.** Promotes freshness to a top-line property. Honest pitch: "the only code search tool that lets your agent *wait* until it's fresh."
2. **#1181 — general mistrust posture.** 3-layer follow-up to indirect-injection cluster: default-on `CQS_TRUST_DELIMITERS`, `_meta.handling_advice` per JSON response, per-chunk `injection_flags`. Frames every cqs response as untrusted-by-default. Cheap; composes with what just landed.
3. **P3 ergonomics cluster (#1137-#1140).** Eval-neutral refactors. Unblocks future features.
   - #1137 — Lift `BatchCmd::is_pipeable` into the registry
   - #1138 — `LlmProvider` resolver via registry slice
   - #1139 — `structural_matchers` shared library
   - #1140 — Embedder preset extras map
4. **README pass** — once #1182 ships, lead with: semantic search + call graphs + **freshness contract** (replacing whatever currently sits at #3). Names the categories: "agent-first," "between-turns consistency," "freshness contract."
5. **P4 auth bugs (#1134-#1136).** `cqs serve` correctness fixes; not high-leverage, but cheap to clear.

## Watch mode arc (positioning lever)

The biggest gap between cqs and similar code-intelligence tools: *easy to index, hard to keep indexed between turns*. IDEs solve "between keystrokes" (continuous time, editor consumer); Sourcegraph solves "between pushes" (discrete time, server consumer); cqs needs to solve "between turns" (discrete time, agent consumer). cqs is the first widely-used tool whose primary consumer is the agent, so it's the first that needs the turn-shaped consistency model.

**Order:**
- #1182 — perfect watch mode (lead)
- Adaptive debounce — idle-flush instead of fixed window
- `cqs status --watch` — daemon health surface
- Whitespace/comment-canonical hash — comment-only edits become free
- Parallel reindex across slots — keeps inactive slots from rotting
- Kill periodic full HNSW rebuild — true delete-and-update on the index

The first item closes the gap; the rest optimize within the closed gap.

## Publishing / vocabulary (compounding)

- **Paper draft (`~/training-data/paper/draft.md`)** — needs revision pass that names the categories. Specifically: agent-first vs human-first code intelligence, the LLM-stage = injection surface principle, between-turns consistency, freshness contract, demote-don't-remove discipline. Higher leverage than another technical feature.
- **README pass** — post-#1182. The framing categories appear in section headers, not just bullets.
- **Blog posts (optional)** — once material is dense enough. Each one names one category and demonstrates with cqs. Lower-priority than paper revision.
- **SECURITY.md** — already names the indirect-injection threat model and the surface table. Keep current as new mitigations land.

## Eval / signal-side levers (parked, but worth tracking)

R@5 routing-side levers are exhausted (per the alpha-routing arc empirical close, 2026-04-21). Future R@5 work should target signal-side levers under paired-reindex protocol:

- **Index-time HyDE re-eval** — never tested at proper N. Regenerate via Claude Batches, reindex with `--hyde-queries`, per-category A/B harness.
- **CodeRankEmbed-137M** — opt-in preset (#1110); wins R@1 on test split at 1/3 the parameters of BGE-large. Could promote to default if a wider eval shows consistent wins; currently opt-in for safety.
- **USearch / SIMD brute-force backends** — plug into `IndexBackend` trait (#1131 closed); gives recall-leaning options for slot A/Bs and small projects.
- **Reranker V2 retrain** — parked; needs 10x more queries OR bge-reranker-large.

## Parked (revisit conditions)

- **`nomic-ai/nomic-embed-code` (7B)** — Phase 2 of code-specific embedder A/B. Skipped because at 7B params, inference cost approaches an LLM call. Revisit if Phase 1 (CodeRankEmbed) shows the code-specialist trade-off is worth pushing.
- **HyDE on v3 dev** — most promising untested representation lever. Per-category routing required.
- **Cross-project refinement** — type-signature matching, import-graph resolution, unified cross-project scoring. Lower priority while single-project workflows are still being tightened.
- **Knowledge-augmented retrieval** — call/type graph as structured filter. Multi-step queries weakest at 28-43% R@1.

## Done summary (recent)

| Arc | Issues | Status |
|---|---|---|
| Indirect-injection threat model | #1166-#1170 | **Closed today (4 PRs merged on main)** |
| P2 cluster | #1130, #1131, #1132 | Closed (PRs #1175, #1173, #1165) |
| Cache+slots infrastructure | #1100, #1105 | Closed in v1.30.0 |
| Three-way embedder A/B | #1109, #1110 | Closed in v1.30.0 |
| v1.29.0 audit close-out | #1095 (umbrella) | Closed in v1.30.0 |
| #956 ExecutionProvider Phase A | #956 (Phase A) | Closed (Phase B/C still open) |

## Closed issues this session (2026-04-27 / 28)

- #1166-#1170 — indirect-injection cluster (5 issues; 4 PRs)
- (cluster comment posted to all 5 issues with PR mappings)

## New issues this session

- **#1181** — general mistrust posture (follow-up to cluster)
- **#1182** — perfect watch mode (positioning lever; prior-art survey in comment)

## Operating notes (carry forward)

- **Defense-in-depth as default discipline.** Even after this cluster, the next feature that adds an LLM stage opens a new injection surface. Plan mitigations on the same per-LLM-stage cadence.
- **Cost frame on every new feature.** "Is this surface worth keeping" before "how do I add a layer." Removal beats mitigation when the capability isn't load-bearing.
- **Watch on the publishing pipeline.** Paper, README, blog posts. The vocabulary compounds; nothing else does as reliably.
