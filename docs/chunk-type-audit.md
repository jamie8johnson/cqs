# Chunk Type Coverage Audit

Full audit of 29 chunk types across 52 languages. 2026-04-08.

## Status: ALL FINDINGS FIXED (PR #852)

Two commits:
1. High priority: Zig Test bug, Test reclassification (13 languages), Namespace (PHP, VB.NET), Constructor (Ruby, Dart, ObjC, JS, TS, Perl), Elixir Test query, Kotlin Constant/Endpoint, Dart Extension
2. Medium + low priority: Rust Impl, C++/CUDA Extern+Constructor, C# Constant, Python Enum, Solidity Constructor+Constant, Julia Constant, OCaml/Gleam/Zig Extern, Lua/R/Bash Variable, Haskell Module, GraphQL Extensions, CSS @supports

## Changes Made

### Bug fix
- Zig: `test_declaration` now sets `ChunkType::Test` (was extracting name but never setting type)

### Test reclassification (19 languages total now)
Previously: Python, Go, JS, TS, Java, C#
Added: Kotlin, VB.NET, F#, Scala, Ruby, PHP, Elixir, Julia, Swift, ObjC, Dart, PowerShell, Perl

### Constructor reclassification
Added: Ruby (initialize), JS/TS (constructor), ObjC (init*), Perl (sub new), Dart (factory), Solidity (constructor), CUDA (no return type)

### Namespace captures
Added: PHP (namespace_definition), VB.NET (namespace_block)

### Extern reclassification
Added: C++/CUDA (linkage_specification parent walk), Zig (extern fn), OCaml (external), Gleam (@external)

### Constant reclassification
Added: Kotlin (const val), C# (const/static readonly), Julia (const), Solidity (constant/immutable)

### Variable reclassification
Changed: Lua, R — non-UPPER module-level assignments now kept as Variable (were dropped)
Added: Bash (export/declare, filtered to top-level only)

### Other
- Rust: impl blocks captured as @impl
- CUDA: concept_definition captured as @trait (parity with C++)
- Haskell: module declaration captured as @module
- GraphQL: type extensions captured as @extension
- Python: class Foo(Enum) → Enum
- Dart: extension → Extension (was Class)
- Kotlin: @GetMapping → Endpoint
- Scala: var → Variable (was incorrectly Constant)
- CSS: @supports captured
- Solidity: constructor + constant/immutable

### New post_process functions (8)
Ruby, Scala, F#, Dart, PowerShell, Bash, Solidity, CUDA

## Remaining (not implemented)
These were assessed as too niche, convention-based, or grammar-unsupported:
- LaTeX: \renewcommand, \newenvironment
- Elm ports as Extern
- Nix constants
- Make sections/test targets
- SQL DECLARE variables
- Protobuf package as Namespace
- Convention-based constructors (Go New*, Zig init(), Gleam)
- Bash bats, R test_that, Erlang EUnit, Haskell HSpec tests
- ObjC NS_ENUM, ObjC struct
- CSS @font-face (node doesn't exist in tree-sitter-css grammar)
- Perl `use constant` (different syntax, not a declaration node)
- GLSL uniform/varying (needs grammar investigation)
- Structured Text Method/Enum (needs grammar investigation)
