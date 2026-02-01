# Project Continuity

## Right Now

**Releasing v0.1.16** - PR #39 open, CI running.

After CI passes:
1. `gh pr merge 39 --squash --delete-branch`
2. `git checkout main && git pull`
3. `git tag v0.1.16 && git push origin v0.1.16`
4. `cargo publish`
5. `gh release create v0.1.16 --generate-notes`

## What's in v0.1.16

- Tracing spans (cmd_index, cmd_query, embed_batch, search_filtered)
- Embedding type encapsulation (private field, as_slice/as_vec/len methods)
- Version check warning when index from different cqs version
- Fixed README cross-file call graph note
- Updated bug report template version placeholder
- Created missing tags v0.1.12-15

## Parked

- C/Java language support
- Code-specific embedding model
- Mock embedder for tests

## Blockers

None.
