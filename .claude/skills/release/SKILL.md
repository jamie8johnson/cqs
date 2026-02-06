---
name: release
description: Release a new version of cqs. Bumps version, updates changelog, publishes to crates.io, creates GitHub release.
disable-model-invocation: true
argument-hint: "[major|minor|patch]"
---

# Release

Release a new version of cqs.

## Arguments

- `$ARGUMENTS` — version bump type: `major`, `minor`, or `patch` (default: `patch`)

## Process

1. **Pre-flight checks**:
   - `git status` — must be clean (no uncommitted changes)
   - `cargo test` — all tests must pass
   - `cargo clippy` — no warnings
   - Confirm on `main` branch

2. **Version bump**:
   - Read current version from `Cargo.toml`
   - Calculate new version based on bump type
   - Update `Cargo.toml` version field
   - Run `cargo check` to update `Cargo.lock`

3. **Docs review**:
   Review each item below. Fix anything stale before cutting the release.
   - `src/lib.rs` doc comment — language count, feature list, API examples still accurate?
   - `Cargo.toml` — `description`, `keywords`, `categories` still reflect the tool?
   - `README.md` — hardcoded versions (schema, install), feature list, usage examples
   - `CONTRIBUTING.md` — Architecture Overview matches current file layout? Any files added/moved/renamed since last release?
   - `SECURITY.md` — threat model current? Any new attack surfaces from this release?
   - `PRIVACY.md` — data handling claims still accurate?
   - GitHub repo description/topics — run `gh repo view` and check. Update with `gh repo edit` if needed.

   Use `git log vPREVIOUS..HEAD --stat` to see what changed and focus review on affected docs.

4. **Changelog**:
   - Read `CHANGELOG.md`
   - Add new section with version and date
   - Summarize changes since last release using `git log` since last tag
   - Categorize: Added, Changed, Fixed, Removed

5. **Commit and tag**:
   - Create branch: `release/vX.Y.Z`
   - Commit: `chore: Release vX.Y.Z`
   - Create PR via PowerShell (WSL): `powershell.exe -Command 'gh pr create ...'`
   - Use `--body-file` for PR body (never inline heredocs)
   - Wait for CI: `powershell.exe -Command 'gh pr checks N --watch'`

6. **After PR merge**:
   - Sync main: `git checkout main && git pull`
   - Tag: `git tag vX.Y.Z`
   - Push tag via PowerShell: `powershell.exe -Command 'cd C:\Projects\cq; git push origin vX.Y.Z'`
   - Publish: `cargo publish`
   - Create GitHub release via PowerShell with `--body-file`

7. **Post-release**:
   - Update PROJECT_CONTINUITY.md with new version
   - Update ROADMAP.md if phase milestones changed

## WSL notes

- All `git push` and `gh` commands go through PowerShell (Windows has credentials)
- Always use `--body-file` for PR/release bodies — never inline heredocs
- Write body content to `/mnt/c/Projects/cq/pr_body.md`, use it, then delete
