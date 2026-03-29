# OpenClaw Security Contribution — Design Spec

> **Status:** Skeleton with agent prompts. Dedicated session required.
>
> **Context:** OpenClaw is the open-source foundation for NVIDIA's NemoClaw enterprise AI agent platform. Their security roadmap (#11829) identifies API key leakage as the top priority. A quality security contribution here has enterprise-grade impact.

## Goal

Trace every path where API keys, tokens, or credentials can leak into LLM prompt context, chat output, or logs in the OpenClaw codebase. File focused, actionable issues with corrective prompts for each confirmed vector.

## Background

OpenClaw issue #11829 identifies three leakage classes:
1. **LLM Provider Keys** — Model catalog with resolved `apiKey` values serialized into prompt context (#11202)
2. **Agent-Accessible Keys** — Agent can read `.env` files and display credentials in chat (#10659)
3. **Channel Tokens** — Telegram, Discord, etc. tokens accessible to the agent runtime

NVIDIA's NemoClaw wraps OpenClaw with a "privacy router" — but the underlying codebase still has these vectors. Fixing them in OpenClaw fixes them in NemoClaw.

## Prerequisites

- Clone OpenClaw: `git clone --depth 1 https://github.com/openclaw/openclaw.git`
- Index with cqs: `cd openclaw && cqs init && cqs index`
- Read their SECURITY.md and CONTRIBUTING.md
- Read issue #11829 fully (the roadmap)
- Read issues #11202 and #10659 (the specific bugs)

## Methodology

For each leakage class, trace the data flow from secret source → dangerous sink.

**Secret sources** (where keys enter the system):
- `models-config.providers.ts` — resolves apiKey from env, config, credential store
- `.env` files — agent can read via file tools
- `openclaw.json` config — contains provider credentials
- OAuth token stores — per-provider token caches
- Channel configs — Telegram bot token, Discord bot token, etc.

**Dangerous sinks** (where keys must NOT reach):
- LLM prompt context (system message, user message, tool results)
- Chat UI output (displayed to user or other agents)
- Log output (console, file logs)
- Serialized state (compaction summaries, memory)
- Tool call arguments (web fetch URLs, file write content)

**Analysis method:**
1. Find every variable assignment that holds a secret (`apiKey`, `token`, `credential`, `secret`, `password`)
2. Trace forward: where does that variable flow? Follow through function calls, object spreads, serialization
3. If it reaches a dangerous sink without redaction → finding
4. Verify the finding is not already fixed or mitigated

## Phases

### Phase 1: Model Catalog Key Leakage (issue #11202)

The most concrete and highest-priority vector.

**Attack chain:** Config resolves provider → provider has `apiKey: "sk-..."` → model catalog object serialized → injected into system prompt → LLM sees the key → agent can extract/exfiltrate it.

**Agent prompt:**

```
Security audit of OpenClaw at /path/to/openclaw.
Focus: API key leakage through model catalog serialization (issue #11202).

READ FIRST:
- src/agents/models-config.providers.ts (where apiKey is resolved)
- src/agents/models-config.merge.ts (where configs are merged)
- src/agents/compaction.ts (where context is serialized)
- Search for every call site of the resolved provider config

TRACE THIS PATH:
1. In models-config.providers.ts, find where apiKey is set on the provider object
2. Follow that provider object through every function that receives it
3. Find where the provider/model config is serialized into a string
4. Find where that string enters the LLM prompt (system message or context)
5. Check: is apiKey redacted/stripped before serialization?

FOR EACH LEAKAGE POINT FOUND:
- Record: file, line, function name
- Record: the exact object path (e.g., provider.apiKey → catalog.providers[0].apiKey → systemPrompt)
- Record: whether any redaction exists (even partial)
- Suggest fix: where to add redaction, what pattern to use

Use grep, cqs callers, and cqs trace to follow the data flow.
Do NOT fix anything — report only.

Output format per finding:
## LKG-N: [Title]
- **Source:** file:line (where the secret enters)
- **Sink:** file:line (where it reaches the prompt/output)
- **Path:** source → fn1() → fn2() → ... → sink
- **Existing mitigation:** none / partial (describe)
- **Suggested fix:** [specific code change]
```

### Phase 2: Agent File Access to Secrets (issue #10659)

The agent runtime can read local files. If `.env` or `openclaw.json` is readable, the agent can extract credentials.

**Agent prompt:**

```
Security audit of OpenClaw at /path/to/openclaw.
Focus: Agent access to secret files (issue #10659).

READ FIRST:
- src/agents/tools/ (all tool implementations, especially file read/web fetch)
- src/agents/pi-embedded-runner/ (the agent runtime)
- Any sandbox or permission system

TRACE:
1. Find the file-read tool implementation
2. Check: does it filter/block reading of .env, openclaw.json, auth token files?
3. Find the web-fetch tool — can it POST to arbitrary URLs (exfiltration)?
4. Check: does the agent see file contents in its context? Can it quote them?
5. Check: does compaction/memory persist file contents that contained secrets?

FOR EACH VECTOR:
- Can the agent read the file? (yes/no, which tool)
- Can the agent display the content? (yes/no, which output path)
- Can the agent exfiltrate it? (yes/no, which network tool)
- Is there a sandbox/allowlist that blocks this? (yes/no, where)

Output as LKG-N findings, same format as Phase 1.
```

### Phase 3: Channel Token Exposure

Channel tokens (Telegram, Discord, MS Teams, etc.) in the runtime.

**Agent prompt:**

```
Security audit of OpenClaw at /path/to/openclaw.
Focus: Channel token exposure to the agent runtime.

READ FIRST:
- extensions/telegram/src/ (bot token handling)
- extensions/discord/src/ (bot token handling)
- extensions/msteams/src/ (auth handling)
- extensions/whatsapp/src/ (session handling)

TRACE for each extension:
1. Where is the bot token/secret loaded?
2. Is it passed to the agent context? (directly or via tool results)
3. Is it visible in logs?
4. Is it stored in compaction/memory state?
5. Is it redacted when displayed in error messages?

Each extension is independent — check all of them.
Output as LKG-N findings.
```

### Phase 4: Issue Filing

After all phases, group findings by severity and file issues on openclaw/openclaw:

- **Critical**: Secret reaches LLM prompt context unredacted → immediate exfiltration risk
- **High**: Secret readable by agent via file tools → requires agent cooperation to exfiltrate
- **Medium**: Secret in logs or error messages → requires log access
- **Low**: Secret in memory/compaction state → requires state inspection

File ONE issue per leakage class (not per finding). Each issue gets:
- The specific findings (file:line, path, impact)
- A corrective agent prompt that fixes all findings in that class
- Expected test: "after fix, grep for apiKey in prompt serialization output should return zero hits"

## Rules

- Read their existing security measures before flagging — they may already have mitigations
- Read #11829 comments for context on what they've already considered
- Don't file findings for things already listed in #11829 — add to the existing roadmap instead
- Every finding must have a concrete data flow trace, not just "this variable is called apiKey"
- Quality over quantity — one well-traced finding is worth more than ten grep hits
- Respect their contribution guidelines (CONTRIBUTING.md)
- Be specific in the corrective prompt — "add `redactApiKey()` before line N" not "consider redacting secrets"

## Anti-patterns (don't file these)

- "apiKey field exists in a TypeScript interface" — that's a type definition, not a leakage
- "test file contains a hardcoded test key" — test keys aren't secrets
- "log message mentions 'api key'" — the string "api key" isn't the actual key
- "config file has an apiKey field" — the config defines where keys go, it doesn't leak them
- Framework callbacks that happen to handle auth tokens — not a vulnerability unless the token reaches the prompt

## Success criteria

- At least one confirmed, novel leakage vector not already in #11829
- Each finding has a traceable source → sink path
- Each issue has a corrective prompt that an agent can execute
- OpenClaw team acknowledges at least one finding as valid
