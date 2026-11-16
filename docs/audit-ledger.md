# Audit ledger

One row per run of the 16-category audit (`/audit`), newest last: the commit that was audited, when, what ran, and where its triage is. This is a run log. It is not the coverage ledger that `docs/plans/2026-06-13-audit-loop.md` sketches.

Use (`.claude/skills/audit/SKILL.md`): before a run, the last row's commit is the start of the churn window, `git log --first-parent <commit>..HEAD`; after a run, append its row in the PR that carries the triage.

| Date | Commit audited | What ran | Triage file |
|------|----------------|----------|-------------|
| 2026-05-08 | `e2f64baa` | 16 categories | `docs/audit-triage-v1.40.0.md` |
| 2026-06-10 | `7284e31e` | 16 categories, in two batches of 8 | `docs/audit-triage-v1.42.0.md` |
| 2026-06-24 | `c519bf9d` (inferred, see below) | 16 categories: find, adversarial verify, triage | `docs/audit-triage.md` |

How the rows above were filled, since they are older than this file:

- 2026-05-08 and 2026-06-10: the triage file states the date and the commit ("Main HEAD").
- 2026-06-24: the triage file states the date and no commit; the commit is inferred. `c519bf9d` (tag `v1.48.0`) is the release commit before the audit's first fix (#2038): `git log --first-parent c519bf9d..2bdd6811` lists that fix and one commit that changed two docs files. Its parent `a94940cd` differs only in docs, the version (in `Cargo.toml` and `Cargo.lock`), the description in `Cargo.toml`, and one `//!` line, and a findings location checked against both trees matches both; the audit may have read either.
- Not in the table: the run of 2026-06-15 (`docs/audit-triage-pre-v1.48.0.md`), whose triage names no commit and which `git log` does not pin to one; and the runs before 2026-05-08, whose triage files name no audited commit (`docs/audit-triage-v1.30.0.md` names only its fix branch's commit). Some findings files of that time do name one, for example `docs/audit-findings-v1.36.2-final.md` (main at `ad921040`) and `docs/audit-findings-v1.38.0-final.md` (`4cfe82ae` and `fe7126c4`, for different categories); no row is made from a findings file.
