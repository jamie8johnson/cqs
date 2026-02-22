---
name: pr
description: Create a pull request with WSL-safe workflow. Uses --body-file to avoid heredoc permission corruption.
disable-model-invocation: true
argument-hint: "[branch-name]"
---

# Create Pull Request

Create a PR from the current branch using WSL-safe patterns.

## Process

1. **Check state**:
   - `git status` — note any uncommitted changes
   - `git log main..HEAD` — review commits to include
   - `git diff main...HEAD --stat` — files changed

2. **Prepare**:
   - If uncommitted changes exist, ask user whether to commit first
   - Ensure branch is pushed: `powershell.exe -Command 'cd C:\Projects\cqs; git push -u origin BRANCH'`

3. **Write PR body** to `/mnt/c/Projects/cqs/pr_body.md`:
   ```markdown
   ## Summary
   - Bullet points summarizing changes

   ## Test plan
   - [ ] cargo test passes
   - [ ] cargo clippy clean
   - [ ] Manual verification steps
   ```

4. **Create PR** via PowerShell:
   ```
   powershell.exe -Command 'cd C:\Projects\cqs; gh pr create --title "..." --body-file pr_body.md'
   ```

5. **Clean up**: Delete `pr_body.md`

6. **Wait for CI**: `powershell.exe -Command 'cd C:\Projects\cqs; gh pr checks N --watch'`

7. **Report**: Show PR URL

## Critical rules

- **ALWAYS use `--body-file`** — never inline heredocs in `gh pr create --body`
- Heredocs get captured as permission entries in `settings.local.json`, corrupting startup
- All `git push` and `gh` commands go through PowerShell (Windows has git credentials)
- main is protected — all changes require PR
