# Chunk Type Coverage Audit

Full audit of 29 chunk types across 52 languages. 2026-04-08.

## Executive Summary

**Biggest systemic gap: Test reclassification.** 15 languages have `test_markers` configured (file-level detection) but never reclassify chunks to `ChunkType::Test`. Only Python, Go, JS, TS, Java, and C# do it properly.

**One bug found:** Zig post_process extracts test names but never sets `ChunkType::Test`.

**Total actionable findings:** 6 high, ~20 medium, ~15 low.

---

## HIGH Priority

### BUG: Zig Test type never set
`post_process_zig_zig` extracts test names from `test_declaration` but never sets `*chunk_type = ChunkType::Test`. One-line fix.

### Missing Test reclassification (13 languages)
These all have `test_markers` for file detection but never set `ChunkType::Test` on individual chunks:

| Language | Pattern to detect | Difficulty | Reference impl |
|----------|------------------|------------|----------------|
| Kotlin | `@Test`, `@ParameterizedTest` annotation | Easy | Copy Java post_process |
| VB.NET | `<Test>`, `<Fact>`, `<Theory>` attribute | Easy | Copy C# post_process |
| F# | `[<Test>]`, `[<Fact>]` attribute | Medium | Needs new post_process |
| Scala | ScalaTest DSL, JUnit `@Test` | Medium | Needs new post_process |
| Ruby | RSpec `describe`/`it`, Minitest `test_` | Medium | Needs new post_process |
| PHP | PHPUnit `testFoo()`, `@test` annotation | Easy | Add to existing post_process |
| Elixir | ExUnit `test "name" do...end` | Easy | Add keyword to existing post_process |
| Julia | `@test`, `@testset` | Medium | Needs new post_process |
| Swift | XCTest `func test*()` | Easy | Add to existing post_process |
| ObjC | XCTest `- (void)test*` | Easy | Add to existing post_process |
| Dart | `test()`, `group()` calls | Medium | Needs new post_process |
| PowerShell | Pester `Describe`/`It`/`Context` | Medium | Needs new post_process |
| Perl | `subtest "name"` | Medium | Needs new post_process |

Also missing but lower priority: Lua (busted), R (test_that), Erlang (EUnit _test()), Haskell (HSpec), OCaml (ppx_inline_test), Gleam, Bash (bats)

### Missing Namespace (2 languages)
| Language | Pattern | Difficulty |
|----------|---------|------------|
| PHP | `namespace App\Controllers;` | Easy — query capture |
| VB.NET | `Namespace ... End Namespace` | Easy — query capture |

### Missing Constructor (3 languages without any post_process)
| Language | Pattern | Difficulty |
|----------|---------|------------|
| Ruby | `initialize` method | Medium — needs new post_process |
| Dart | constructor declarations | Medium — needs new post_process |
| ObjC | `init` methods | Easy — add to existing post_process |

---

## MEDIUM Priority

### Constructor gaps (languages with existing post_process)
| Language | Pattern |
|----------|---------|
| JS/TS | `constructor()` in classes |
| Perl | `sub new { ... }` |
| Solidity | `constructor()` function |

### Constant gaps
| Language | Pattern |
|----------|---------|
| Kotlin | `const val` declarations (currently Property) |
| C# | `const` and `static readonly` fields |
| Julia | `const FOO = 42` |
| Perl | `use constant FOO => 42;` |

### Extern gaps
| Language | Pattern |
|----------|---------|
| C/C++/CUDA | `extern "C" { }` (linkage_specification) |
| Zig | `extern fn` / `extern "c" fn` |
| OCaml | `external` declarations |
| Gleam | `@external` attribute |

### Endpoint gaps
| Language | Pattern |
|----------|---------|
| Kotlin | `@GetMapping`, `@PostMapping` (same as Java) |
| VB.NET | `<HttpGet>`, `<HttpPost>` (same as C#) |

### Other medium gaps
| Language | Type | Pattern |
|----------|------|---------|
| Rust | Impl | `impl Foo` / `impl Trait for Foo` blocks |
| CUDA | Constructor | Port from C++ post_process |
| CUDA | Trait | `concept_definition` (present in C++ query, absent in CUDA) |
| Dart | Extension | `extension_declaration` captured as Class, should be Extension |
| Python | Enum | `class MyEnum(Enum):` via base class detection |
| Lua | Variable | module-level non-UPPER assignments (currently dropped) |
| Lua | Method | `Foo:bar()` colon syntax |
| Zig | Constant/Variable | non-type `const`/`var` dropped by post_process |
| GLSL | Variable | uniform/varying/in/out declarations (shader interface) |
| Bash | Variable | `export`/`declare` assignments |
| R | Variable | non-constant non-R6 assignments (currently dropped) |
| Structured Text | Method | `method_definition` captured as Function |
| Structured Text | Enum | enum type declarations |
| Haskell | Module | `module Foo where` |
| CSS | @font-face | `font_face_statement` node |
| CSS | @supports | `supports_statement` node |
| GraphQL | Type extensions | `extend type User { ... }` |

---

## LOW Priority

- LaTeX: \renewcommand, \newenvironment
- Elm ports as Extern
- Nix constants
- Make sections/test targets
- SQL DECLARE variables
- Protobuf package as Namespace
- Various convention-based constructors (Go New*, Zig init(), Gleam)
- Bash bats tests, R test_that, Erlang EUnit, Haskell HSpec tests
- ObjC NS_ENUM, ObjC struct
- Scala var_definition captured as @const (mutable as constant)

---

## Languages with best coverage (no real gaps)
- Go, Java, C#, TypeScript, Python, Razor, Protobuf, JSON, TOML, YAML, INI, XML, HCL

## Languages with worst coverage relative to their features
- Ruby (no post_process at all — missing Constructor, Test)
- Dart (missing Constructor, Test, Extension reclassification)
- Lua (missing Method, Variable, Test)
- Perl (missing Constructor, Test, Constant)
- Zig (Test bug + Constant/Variable/Extern gaps)
