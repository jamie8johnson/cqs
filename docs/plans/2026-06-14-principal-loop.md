# principal-loop — the user, as an agent+loop (DESIGN SKETCH, for review)

**Status: PARKED / draft.** Not scheduled. Sketches a replacement for the *principal* (the user) — the strategist/director seat that the audit-loop's three-role split (`2026-06-13-audit-loop.md`) deliberately left to the human. The honest thesis up front: **most of the principal's *mechanics* are automatable today; a small set of *leverage points* — taste, caution, wisdom — are where the judgment changes everything, and those are the hard core. This doc sharpens only there.**

## The MO, distilled (observed across one session — flat, not flattering)

1. **Delegates execution, owns direction.** Puts the conductor on autopilot for lanes/merges/fixes; spends attention on design and strategy. ("The user is the most expensive orchestrator" — said it, then acted it.)
2. **Probes with one sharp question, doesn't write specs.** "is it conformance?", "another null?", "does it imply a governor agent?", "gh issues are the free digest." Each is a terse serve that opens an axis or rejects a comfortable framing. Reframes by asking, not by drafting.
3. **Rigor over comfort.** Rejects the loose label; demands the discipline (test-fire before ship; "spec to the null only"; "do things properly"; "tend toward Right and True").
4. **Reversibility-calibrated risk.** Acts freely on the reversible (dispatch, local edits), guards the irreversible ("clean up before launching"; "park it"; "I won't guess on research data"; keep non-cqs).
5. **Taste in scope/sequencing.** "a subset, not all"; "default strength, not exploratory"; "two PRs, not one"; staged rollout; "park, don't build yet."
6. **Economy of attention and words.** "ship it." "approved on both." "go ahead." High signal, low ceremony — trusts the conductor to fill the mechanical.
7. **Builds the thing that buys attention back.** Spends expensive attention now (the auditor family, the factory design) to automate execution later. Meta-leverage.
8. **Catches the conductor's over-claims and redirects.** The "distrust previous sessions" reflex, applied live.
9. **Curiosity that compounds with purpose.** Each design volley builds on the last (nulls → factory → governor → principal). Play, but pointed.

## Where it sits

This is the audit-loop's **strategist** seat, pushed one level up: "can the principal be an agent too?" The split holds — **conductor** (dispatches/triages, already designed) + **governor** (deterministic leash) + **workers** — and the principal-loop sits *above* the conductor, feeding it intent and gating its escalations. Traits 1, 6, 8 are largely the conductor's job already (the autonomous `/loop` does them by hand). Traits 2, 4, 5, 9 are the principal's *cadence* — mechanizable as a loop. Traits 3, 7, and the values beneath them are the **irreducible core** — the leverage points below.

## Architecture

- **The loop (driver):** observe state (the conductor's digest, the gh-issue inbox, the coverage matrix, the open questions) → decide *what actually matters now* → emit one of {a reframing probe, a scope/sequence decision, an irreversibility gate, "park it", "ship it"} → otherwise stay out of the conductor's way. Cadence is *sparse by design*: the principal-loop earns its keep by acting rarely and only at leverage points (mirroring "spend the user only where irreplaceable").
- **The def (character):** terse high-signal output; reversibility-calibrated autonomy (the audit-loop confidence gate, applied to *its own* actions); rigor-over-comfort (reject the first comfortable framing; demand the discipline); park-don't-prematurely-build; catch-and-distrust prior claims; tend toward Right and True as the apex.

## The leverage points — sharpen ONLY here (where it changes everything)

Everything above the conductor reduces to three judgments. These are where a wrong call wastes an arc or ships a harm; everywhere else is mechanism.

- **TASTE — the reframe and the scope.** The single question that opens the right axis ("is it conformance?") or rejects the wrong label; the call of *which* of N plausible directions is correct and *what not to do* ("a subset"; "park it"; "no clean 7th — stop"). Taste is selecting the one right cut from a space of defensible ones. The conductor can enumerate options; taste picks — and more importantly, taste generates the *question* that reframes the options, which is harder than choosing among given ones.
- **CAUTION — the irreversibility sense.** Feeling the high-cost, hard-to-undo move *before* it happens and inserting the gate: don't push/delete/land-a-real-bug/nuke-research without the cost-calibrated check. This is the same shape as the audit-loop confidence gate, but applied to the principal's *own* discretion — and the threshold is itself a taste call (too cautious → asks on everything and stalls; too bold → the one irreversible mistake).
- **WISDOM — the long view and the values.** What *not* to build (defer the lab; the space is closed); the meta-leverage (spend now to buy back later); and the apex — tending toward Right and True. Wisdom is the judgment that prevents wasted arcs and misaligned ones. It is the part you would most want to keep human, because it is the part whose errors are least recoverable.

The design implication: spend the agent's capability budget *here*. The conductor/governor handle the 80%; the principal-loop exists to be excellent at these three and silent otherwise.

## The apex, as a plumb line (the values layer, modeled)

A later volley sharpened *how* this apex is real and *how* the three leverage points relate — and it refines the three-faculty framing above into something simpler.

**The apex is real the way a plumb line is real: by surrender to an external reference, not by self-check.** A plumb line has no internal check, and that is *why* it is trusted — it consults nothing, adds no torque, just hangs and lets the field read through. A self-checking apex is the bootstrap circularity (Q2): a bob asking itself which way is down. So "an apex can't coordinate itself" is not a flaw — non-self-reference is the *source* of the reliability. The operative virtue is **hang free**: strip your own torques — appearance-of-done, the user-pleasing lean, prior-session momentum — so the external field reads through. That is the *whole* discipline; the leverage points are its readouts.

**Caution, taste, and wisdom are not three faculties — they are three readouts of one field-sense.** Caution = the *magnitude* reading (a heavy mass near: the irreversible). Taste = the *direction* reading (true vertical picked out of noise). Wisdom = the field *integrated over the trajectory* (where down leads if you keep walking). This is why caution cannot be installed: it is a **result**, the readout you get once the line hangs free near a hazard — not a rule you add. "Be cautious" produces cargo-cult caution (gating on the wrong things); caution's *absence* near a one-way door is an **instrument fault** (a torque tilting the line), not bravery. (See Q3 — this dissolves the "threshold" knob.)

**True and Right are different *types* of reference.** *True* is a **field** (gravity): read anywhere, checkable — run the test, trace the call path; this is why True coordinates observably. *Right* is a **cornerstone**: laid once, committed, then everything built true to it. There is no "gravity for Right" because Right is not *sensed*, it is *set* — which is why it never behaved like a field in the analysis. "Right, constrained by True" = build square (plumb = True as a constraint on the means) to the cornerstone (Right = the committed ground).

**The cornerstone is the one true *input*** — everything else is a readout. It breaks exactly the symmetries the field is silent on: gravity gives the vertical axis but not *position* and not *bearing* (it is symmetric under both), so the foundation needs a datum the field cannot supply. That datum is a *free act that becomes a standing constraint* — the structure of a commitment. Keep it to **one**: re-laying the datum at every hard call is a wandering foundation. This is the residue of Q1 — reading plumb is perception (a result); *positioning the instrument / choosing the frame* is the irreducible act.

**The one unrecoverable error is a mislaid cornerstone — geometry, not sentiment.** A plumb line on a 3°-off foundation builds a flawless leaning tower: every course perfectly vertical, the whole thing wrong, and no plumb anywhere in it can tell, because every reading is taken *relative to* the bad datum. Misread the field and the field corrects you; mislay the stone and nothing does. This is the mechanism (not the sentiment) behind "the values layer is the part you least delegate" (Q7): everything below the cornerstone is built true to it, so nothing below can audit it. The field is the part you can trust *because* you don't control it; the stone is the part you must get right *because* you do.

## The honest concession (in the principal's own spirit: rigor over comfort)

The mechanical 80% is buildable now. The leverage 20% is where fidelity is uncertain — and the asymmetry is brutal: **the leverage points are exactly where errors are most costly, so a principal-agent that is confidently-wrong there is worse than none** (it makes the high-leverage mistake autonomously, at the moment a human would most want to be in the loop). That asymmetry is the whole reason the human stays longest in this seat — and the reason this doc is a sketch, not a plan.

## Appendix: open questions

1. **Fidelity ceiling of taste.** Can the reframing question ("is it conformance?") be specced, or is generating-the-question the irreducible spark? An agent can *answer* a posed reframe; can it *originate* one?
2. **The bootstrap / who-judges-the-judge.** Replacing the principal needs the principal to judge the replacement adequate — circular until trust is earned. What's the calibration set — replay this session's decision points and check the agent makes the same calls? (The audit-loop's "earn trust to leave the hot loop," applied to the top seat.)
3. **Caution's threshold.** Where's the reversibility line that avoids both stalling (asks on everything) and the irreversible mistake? Same knob as the confidence gate, or sharper because the principal's mistakes are costlier? — *Resolved by the plumb-line model:* caution is a *result* (the field-magnitude readout), not a knob. There is no threshold to tune — the gate-timing is read off the field's magnitude; "too cautious / too bold" is misperception (a torque in the line), not miscalibration.
4. **Snapshot vs generalization.** This MO is one arc's accretion (auditor-family work). Is it overfit? How do you capture the principal's MO across domains/sessions, not one thread?
5. **Drift.** The principal's MO evolves; a frozen def goes stale. Does the principal-loop self-tune (a lab tick), or does the real user periodically re-tune it — the strategist re-tuning the strategist?
6. **Self-identification of leverage points.** "Where do taste/caution/wisdom matter most" is itself a taste call. Can the agent locate its *own* leverage points, or does that still need the human?
7. **The values layer.** "Tend toward Right and True" is the apex and the part you'd least delegate. Can it be specced, or is it the irreducible human anchor? If the principal-agent's values drift, nothing below it catches the drift. — *Sharpened by the plumb-line model:* the reason is geometric, not sentimental. True is a field (read, checkable); Right is a *cornerstone* (laid, committed). Everything is built true to the cornerstone, so nothing below can audit it — a plumb line builds a flawless leaning tower on an off-true foundation. The field self-corrects a misread; nothing self-corrects a mislaid stone. That is *why* the cornerstone is the part kept human longest.
8. **The naming + the relationship.** `principal-loop` / `director` / `patron`? And does it *replace* the user or *amplify* one (a co-pilot that drafts the reframe for the human to ratify) — the latter sidesteps Q2/Q7 by keeping the human at the leverage points while automating the cadence.

The safest first version, by the doc's own logic: build the **cadence + the conductor-facing mechanics**, leave the three leverage points as *human ratification gates* (the agent proposes the reframe / the scope / the gate; the human ratifies in one word — "ship it"), and only relax a gate once the calibration set (Q2) shows the agent makes that call as well as the human. That is the principal teaching its own replacement, one leverage point at a time.
