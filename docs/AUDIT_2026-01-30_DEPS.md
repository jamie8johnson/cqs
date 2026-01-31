# Dependency Audit - 2026-01-30

Audit of all dependencies in DESIGN.md v0.7.0 for CVEs, maintenance status, license compatibility, and version conflicts.

## Summary

| Category | Count | Status |
|----------|-------|--------|
| CVEs/Security Advisories | 1 | RESOLVED (chrono patched) |
| Unmaintained/Abandoned | 2 | CONCERN (glob, fs2) |
| License Issues | 0 | All MIT/Apache-2.0 compatible |
| Version Conflicts | 0 | None detected |
| Version Corrections | 0 | tree-sitter 0.26 verified |

---

## New Dependencies (v0.7.0)

### walkdir = "2"
- **Status**: MAINTAINED
- **Maintainer**: Andrew Gallant (BurntSushi)
- **Last Release**: v2.5.0 (over 1 year ago)
- **Downloads**: 318+ million
- **CVEs**: None
- **License**: MIT/UNLICENSE
- **Assessment**: SAFE - Mature, stable crate. Infrequent releases are normal for feature-complete libraries.
- **Source**: [GitHub](https://github.com/BurntSushi/walkdir), [crates.io](https://crates.io/crates/walkdir)

### fs2 = "0.4"
- **Status**: UNMAINTAINED - Last release 7+ years ago
- **Maintainer**: Original author appears inactive
- **Last Release**: v0.4.3 (2017)
- **CVEs**: None known
- **License**: MIT/Apache-2.0
- **Assessment**: CONCERN - Works but no active maintenance
- **Alternative**: Consider `fs4` crate - fork with async support and active maintenance
- **Source**: [crates.io](https://crates.io/crates/fs2), [fs4 alternative](https://lib.rs/crates/fs4)

### ctrlc = "3"
- **Status**: ACTIVELY MAINTAINED
- **Maintainer**: Antti Keranen
- **Last Release**: v3.5.1 (November 2025)
- **Downloads**: 68+ million
- **CVEs**: None
- **License**: MIT/Apache-2.0
- **Assessment**: SAFE - Active development with 2025 releases including platform support updates
- **Source**: [GitHub](https://github.com/Detegr/rust-ctrlc), [crates.io](https://crates.io/crates/ctrlc)

### rayon = "1"
- **Status**: ACTIVELY MAINTAINED
- **Maintainer**: Rayon team (rayon-rs)
- **Last Release**: v1.11.0 (2025)
- **Downloads**: 266+ million
- **CVEs**: None
- **License**: MIT/Apache-2.0
- **Assessment**: SAFE - Industry standard for data parallelism, actively maintained
- **Source**: [GitHub](https://github.com/rayon-rs/rayon), [crates.io](https://crates.io/crates/rayon)

### chrono = "0.4"
- **Status**: MAINTAINED (patched)
- **Maintainer**: chrono team
- **CVEs**: RUSTSEC-2020-0159 - Potential segfault in `localtime_r` invocations
  - **Status**: RESOLVED in v0.4.20+
  - **Affected**: Versions < 0.4.20 on Unix platforms
  - **Risk**: Segfault if environment variable set in different thread
  - **Fix**: Use chrono >= 0.4.20 (current is 0.4.x)
- **License**: MIT/Apache-2.0
- **Assessment**: SAFE if >= 0.4.20 - Recommend pinning minimum version
- **Source**: [RustSec Advisory](https://rustsec.org/advisories/RUSTSEC-2020-0159.html), [crates.io](https://crates.io/crates/chrono)

---

## Existing Dependencies

### CLI
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| clap | 4 | MAINTAINED | None | MIT/Apache-2.0 | Industry standard |

### Parsing
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| tree-sitter | 0.26 | MAINTAINED | None | MIT | **VERIFIED EXISTS** - v0.26.3 released Dec 2025 |
| tree-sitter-rust | 0.23 | MAINTAINED | None | MIT | Compatible with tree-sitter 0.26 |
| tree-sitter-python | 0.23 | MAINTAINED | None | MIT | Compatible with tree-sitter 0.26 |
| tree-sitter-typescript | 0.23 | MAINTAINED | None | MIT | Compatible with tree-sitter 0.26 |
| tree-sitter-javascript | 0.25 | MAINTAINED | None | MIT | Compatible with tree-sitter 0.26 |
| tree-sitter-go | 0.23 | MAINTAINED | None | MIT | Compatible with tree-sitter 0.26 |

**Note**: tree-sitter 0.26.x is confirmed to exist on crates.io (released Dec 2025), resolving previous audit concern.

### ML
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| ort | 2.0.0-rc.11 | PRE-RELEASE | None | MIT/Apache-2.0 | RC version, less scrutiny than stable. Update when 2.0 stable releases |
| tokenizers | 0.22 | MAINTAINED | None | Apache-2.0 | HuggingFace maintained, v0.22.2 latest |
| hf-hub | 0.4 | MAINTAINED | None | MIT/Apache-2.0 | HuggingFace maintained |
| ndarray | 0.16 | MAINTAINED | None | MIT/Apache-2.0 | Widely used |

### Async
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| tokio | 1 | MAINTAINED | None | MIT | Industry standard |

### Storage
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| rusqlite | 0.31 | MAINTAINED | None | MIT | Old RUSTSEC-2021-0128 affects <=0.26.1, 0.31 unaffected |

### Serialization
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| serde | 1 | MAINTAINED | None | MIT/Apache-2.0 | Industry standard |
| serde_json | 1 | MAINTAINED | None | MIT/Apache-2.0 | Industry standard |
| toml | 0.8 | MAINTAINED | None | MIT/Apache-2.0 | Active development |

### Utilities
| Crate | Version | Status | CVEs | License | Notes |
|-------|---------|--------|------|---------|-------|
| blake3 | 1 | MAINTAINED | None | CC0/Apache-2.0 | Active development |
| glob | 0.3 | UNMAINTAINED | None | MIT/Apache-2.0 | **No updates since 2016** |
| colored | 2 | MAINTAINED | None | MPL-2.0 | License compatible |
| indicatif | 0.17 | MAINTAINED | None | MIT | Active development |
| anyhow | 1 | MAINTAINED | None | MIT/Apache-2.0 | Industry standard |
| thiserror | 2 | MAINTAINED | None | MIT/Apache-2.0 | v2.0 released, requires rustc 1.61+ |
| dirs | 5 | MAINTAINED | None | MIT/Apache-2.0 | Active |
| tracing | 0.1 | MAINTAINED | None | MIT | Industry standard |

---

## Issues Requiring Action

### HIGH PRIORITY

#### 1. fs2 Unmaintained (7+ years)
- **Risk**: No bug fixes or security patches available
- **Recommendation**: Replace with `fs4` crate
  - Fork of fs2 with async support
  - Actively maintained
  - Drop-in replacement for sync API

```toml
# Change from:
fs2 = "0.4"
# To:
fs4 = "0.10"
```

#### 2. glob 0.3 Unmaintained (8+ years)
- **Risk**: No updates since January 2016
- **Recommendation**: Replace with `globset` or `globwalk`
  - `globset` is part of ripgrep, actively maintained by BurntSushi
  - More features and better performance
  - Note: Pulls in regex crate (larger dependency tree)

```toml
# Change from:
glob = "0.3"
# To:
globset = "0.4"  # or globwalk = "0.9"
```

### MEDIUM PRIORITY

#### 3. chrono Minimum Version
- **Risk**: Versions < 0.4.20 have segfault CVE
- **Recommendation**: Pin minimum version

```toml
# Change from:
chrono = "0.4"
# To:
chrono = "0.4.20"
```

#### 4. ort Pre-release
- **Risk**: RC versions receive less security scrutiny
- **Recommendation**: Monitor for stable 2.0 release, update when available
- **Current**: Acceptable for MVP, track ort releases

---

## Version Conflict Analysis

No conflicts detected. All dependencies use compatible version ranges:

- tree-sitter grammars (0.23-0.25) compatible with tree-sitter 0.26
- All serde ecosystem on v1.x
- tokio ecosystem on v1.x
- No overlapping transitive dependencies with conflicting versions

---

## License Compatibility Matrix

All dependencies are compatible with MIT/Apache-2.0 licensing:

| License | Crates | Compatible |
|---------|--------|------------|
| MIT | Most crates | YES |
| Apache-2.0 | Most crates | YES |
| MIT/Apache-2.0 dual | serde, tokio, etc. | YES |
| MPL-2.0 | colored | YES (file-level copyleft) |
| UNLICENSE | walkdir | YES (public domain) |
| CC0 | blake3 | YES (public domain) |

**Conclusion**: No license issues. Project can be MIT/Apache-2.0 dual-licensed.

---

## Previous Audit Issues - Resolution Status

From AUDIT_2026-01-31_v2.md:

| Issue | Previous Status | Current Status |
|-------|-----------------|----------------|
| D1: glob 0.3 unmaintained | OPEN | CONFIRMED - Still unmaintained |
| D2: tree-sitter 0.26 may not exist | OPEN | RESOLVED - v0.26.3 exists |
| D3: rusqlite 0.31 outdated | OPEN | ACCEPTABLE - No security issues |
| D4: indicatif 0.17.12 yanked | OPEN | CHECK - Use 0.17.x latest |

---

## Recommendations Summary

### Must Do (Before MVP)
1. Replace `fs2` with `fs4` - unmaintained dependency with modern alternative
2. Pin `chrono >= 0.4.20` - avoid CVE-affected versions

### Should Do (Phase 1)
3. Replace `glob` with `globset` - unmaintained with better alternative
4. Update to `ort` stable when 2.0 releases

### Monitor
5. Track ort 2.0 stable release
6. Watch for rusqlite updates (not urgent)

---

## Audit Methodology

- RustSec Advisory Database: https://rustsec.org/advisories/
- crates.io version history and download statistics
- GitHub repository activity and maintenance status
- Web search for recent security discussions (2025-2026)

**Audit Date**: 2026-01-30
**Design Version**: 0.7.0-draft
**Auditor**: Claude (automated dependency analysis)
