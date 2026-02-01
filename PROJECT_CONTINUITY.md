# Project Continuity

## Right Now

**PR #45** - Hunches + Scars + Security + Optimizations

https://github.com/jamie8johnson/cqs/pull/45

Branch: `feat/hunches-indexing` - ready to merge

### Done this session:
- Scars as indexed entities (entity type 3)
- Phase 1 optimizations (release profile, zero-copy, lazy ONNX)
- "Passive laziness" scar added
- Discussed encryption at rest

### Encryption design (not implemented yet):
- SQLCipher + OS keyring for transparent encryption
- `--encrypt` flag on init
- KeyProvider trait for future backends (Azure KV, AWS KMS, Vault)
- Feature flag: `encrypt = ["rusqlite/bundled-sqlcipher", "keyring"]`

## Key Insight

cqs is Tears - context persistence for AI collaborators.

| Entity | Purpose |
|--------|---------|
| 1. Code | Functions, methods |
| 2. Hunch | Soft observations |
| 3. Scar | Failed approaches |

## Parked

- Encryption implementation (KeyProvider trait stubbed)
- Phase 2-3 optimizations (diminishing returns)
- C/Java language support

## Blockers

None. PR #45 is ready to merge.

## Next

1. Merge PR #45
2. Optionally implement encryption feature
