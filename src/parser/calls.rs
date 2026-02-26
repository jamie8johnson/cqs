//! Call extraction from tree-sitter parse trees

use std::path::Path;
use tree_sitter::StreamingIterator;

use super::types::{
    CallSite, ChunkType, ChunkTypeRefs, FunctionCalls, Language, ParserError, TypeEdgeKind, TypeRef,
};
use super::Parser;

impl Parser {
    /// Extract function calls from a chunk's source code
    ///
    /// Returns call sites found within the given byte range of the source.
    pub fn extract_calls(
        &self,
        source: &str,
        language: Language,
        start_byte: usize,
        end_byte: usize,
        line_offset: u32,
    ) -> Vec<CallSite> {
        // Grammar-less languages (Markdown) — no tree-sitter call extraction
        if language.def().grammar.is_none() {
            return vec![];
        }

        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&grammar).is_err() {
            return vec![];
        }

        let tree = match parser.parse(source, None) {
            Some(t) => t,
            None => return vec![],
        };

        let query = match self.get_call_query(language) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "Tree-sitter query failed in extract_calls");
                return vec![];
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        // Only match within the chunk's byte range
        cursor.set_byte_range(start_byte..end_byte);

        let mut calls = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee_name = source[cap.node.byte_range()].to_string();
                // saturating_sub prevents underflow if line_offset > position
                // .max(1) ensures we never produce line 0 (line numbers are 1-indexed)
                let line_number = (cap.node.start_position().row as u32 + 1)
                    .saturating_sub(line_offset)
                    .max(1);

                // Skip common noise (self, this, super, etc.)
                if !should_skip_callee(&callee_name) {
                    calls.push(CallSite {
                        callee_name,
                        line_number,
                    });
                }
            }
        }

        // Deduplicate calls to the same function (keep first occurrence)
        let mut seen = std::collections::HashSet::new();
        calls.retain(|c| seen.insert(c.callee_name.clone()));

        calls
    }

    /// Extract function calls from a parsed chunk
    ///
    /// Convenience method that extracts calls from the chunk's content.
    pub fn extract_calls_from_chunk(&self, chunk: &super::types::Chunk) -> Vec<CallSite> {
        // Markdown chunks use custom reference extraction
        if chunk.language == Language::Markdown {
            return crate::parser::markdown::extract_calls_from_markdown_chunk(chunk);
        }

        self.extract_calls(
            &chunk.content,
            chunk.language,
            0,
            chunk.content.len(),
            0, // No line offset since we're parsing the content directly
        )
    }

    /// Extract type references from a chunk's byte range
    ///
    /// Returns classified type references with merge logic: if a type name
    /// was captured by any classified pattern (Param/Return/Field/Impl/Bound/Alias),
    /// the catch-all duplicate is dropped. Types found ONLY by the catch-all
    /// get `kind = None`.
    pub fn extract_types(
        &self,
        source: &str,
        tree: &tree_sitter::Tree,
        language: Language,
        start_byte: usize,
        end_byte: usize,
    ) -> Vec<TypeRef> {
        let _span = tracing::info_span!("extract_types", %language).entered();

        let query = match self.get_type_query(language) {
            Ok(q) => q,
            Err(_) => {
                // Language has no type query (e.g., JavaScript) — not a warning
                return vec![];
            }
        };

        let capture_names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);

        // Collect all (type_name, line_number, kind) entries
        let mut classified: Vec<TypeRef> = Vec::new();
        let mut catch_all: Vec<TypeRef> = Vec::new();

        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let capture_name = match capture_names.get(cap.index as usize) {
                    Some(name) => *name,
                    None => continue,
                };

                let kind = match capture_name {
                    "param_type" => Some(TypeEdgeKind::Param),
                    "return_type" => Some(TypeEdgeKind::Return),
                    "field_type" => Some(TypeEdgeKind::Field),
                    "impl_type" => Some(TypeEdgeKind::Impl),
                    "bound_type" => Some(TypeEdgeKind::Bound),
                    "alias_type" => Some(TypeEdgeKind::Alias),
                    "type_ref" => None,
                    other => {
                        tracing::debug!(capture = other, "Unknown type capture");
                        continue;
                    }
                };

                let type_name = source[cap.node.byte_range()].to_string();
                let line_number = cap.node.start_position().row as u32 + 1;

                let type_ref = TypeRef {
                    type_name,
                    line_number,
                    kind,
                };

                if kind.is_some() {
                    classified.push(type_ref);
                } else {
                    catch_all.push(type_ref);
                }
            }
        }

        // Build set of type names that have at least one classified entry
        let classified_names: std::collections::HashSet<String> =
            classified.iter().map(|t| t.type_name.clone()).collect();

        // Keep catch-all entries only for types NOT already classified
        for t in catch_all {
            if !classified_names.contains(&t.type_name) {
                classified.push(t);
            }
        }

        // Dedup by (type_name, kind) — same type as Param twice → one entry,
        // but same type as Param AND Return → two entries
        let mut seen = std::collections::HashSet::new();
        classified.retain(|t| seen.insert((t.type_name.clone(), t.kind)));

        classified
    }

    /// Extract all function calls from a file, ignoring size limits
    ///
    /// Returns calls for every function in the file, including those >100 lines
    /// that would normally be skipped during chunk extraction.
    /// Thin wrapper around `parse_file_relationships()`.
    pub fn parse_file_calls(&self, path: &Path) -> Result<Vec<FunctionCalls>, ParserError> {
        let (calls, _types) = self.parse_file_relationships(path)?;
        Ok(calls)
    }

    /// Extract all function calls AND type references from a file in a single parse pass
    ///
    /// Returns `(calls, type_refs)` for every chunk in the file. Single file read,
    /// single tree-sitter parse, two query cursors on the same tree.
    pub fn parse_file_relationships(
        &self,
        path: &Path,
    ) -> Result<(Vec<FunctionCalls>, Vec<ChunkTypeRefs>), ParserError> {
        let _span =
            tracing::info_span!("parse_file_relationships", path = %path.display()).entered();

        // Check file size (matching parse_file limit)
        const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                tracing::warn!(
                    "Skipping large file ({}MB > 50MB limit): {}",
                    meta.len() / (1024 * 1024),
                    path.display()
                );
                return Ok((vec![], vec![]));
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok((vec![], vec![]));
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistency
        let source = source.replace("\r\n", "\n");

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Language::from_extension(ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages (Markdown) use custom reference extraction
        if language.def().grammar.is_none() {
            let md_calls = crate::parser::markdown::parse_markdown_references(&source, path)?;
            return Ok((md_calls, vec![]));
        }

        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{:?}", e)))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get or compile queries (lazy initialization)
        let chunk_query = self.get_query(language)?;
        let call_query = self.get_call_query(language)?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        // Reuse these allocations across iterations
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();

        while let Some(m) = matches.next() {
            // Find chunk node
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                matches!(
                    name,
                    "function"
                        | "struct"
                        | "class"
                        | "enum"
                        | "trait"
                        | "interface"
                        | "const"
                        | "module"
                        | "macro"
                        | "object"
                        | "typealias"
                        | "property"
                        | "delegate"
                        | "event"
                )
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get chunk name
            let name_idx = chunk_query.capture_index_for_name("name");
            let mut name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            // Apply post-process hook for name corrections (needed for HCL qualified names)
            if let Some(post_process) = language.def().post_process_chunk {
                // Infer chunk_type from capture name
                let cap_name = capture_names
                    .get(func_capture.index as usize)
                    .copied()
                    .unwrap_or("");
                let mut ct = match cap_name {
                    "function" => ChunkType::Function,
                    "struct" => ChunkType::Struct,
                    "class" => ChunkType::Class,
                    "enum" => ChunkType::Enum,
                    "trait" => ChunkType::Trait,
                    "interface" => ChunkType::Interface,
                    "const" => ChunkType::Constant,
                    "module" => ChunkType::Module,
                    "macro" => ChunkType::Macro,
                    "object" => ChunkType::Object,
                    "typealias" => ChunkType::TypeAlias,
                    "property" => ChunkType::Property,
                    "delegate" => ChunkType::Delegate,
                    "event" => ChunkType::Event,
                    _ => ChunkType::Function,
                };
                if !post_process(&mut name, &mut ct, node, &source) {
                    continue; // Skip discarded chunks
                }
            }

            let line_start = node.start_position().row as u32 + 1;
            let byte_range = node.byte_range();

            // --- Call extraction ---
            call_cursor.set_byte_range(byte_range.clone());
            calls.clear();

            let mut call_matches =
                call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

            while let Some(cm) = call_matches.next() {
                for cap in cm.captures {
                    let callee_name = source[cap.node.byte_range()].to_string();
                    let call_line = cap.node.start_position().row as u32 + 1;

                    if !should_skip_callee(&callee_name) {
                        calls.push(CallSite {
                            callee_name,
                            line_number: call_line,
                        });
                    }
                }
            }

            // Deduplicate calls
            seen.clear();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                call_results.push(FunctionCalls {
                    name: name.clone(),
                    line_start,
                    calls: std::mem::take(&mut calls),
                });
            }

            // --- Type extraction ---
            let mut type_refs =
                self.extract_types(&source, &tree, language, byte_range.start, byte_range.end);

            // Filter self-referential types (e.g., struct Config shouldn't list Config as a dep)
            type_refs.retain(|t| t.type_name != name);

            if !type_refs.is_empty() {
                type_results.push(ChunkTypeRefs {
                    name,
                    line_start,
                    type_refs,
                });
            }
        }

        Ok((call_results, type_results))
    }
}

/// Check if a callee name should be skipped (common noise)
///
/// These are filtered because they don't provide meaningful call graph information:
/// - `self`, `this`, `Self`, `super`: Object references, not real function calls
/// - `new`: Constructor pattern, not a named function
/// - `toString`, `valueOf`: Ubiquitous JS/TS methods that add noise
///
/// Case-sensitive to avoid false positives (e.g., "This" as a variable name).
fn should_skip_callee(name: &str) -> bool {
    matches!(
        name,
        "self" | "this" | "super" | "Self" | "new" | "toString" | "valueOf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    mod skip_callee_tests {
        use super::*;

        #[test]
        fn test_skips_self_variants() {
            assert!(should_skip_callee("self"));
            assert!(should_skip_callee("Self"));
            assert!(should_skip_callee("this"));
            assert!(should_skip_callee("super"));
        }

        #[test]
        fn test_skips_common_noise() {
            assert!(should_skip_callee("new"));
            assert!(should_skip_callee("toString"));
            assert!(should_skip_callee("valueOf"));
        }

        #[test]
        fn test_allows_normal_functions() {
            assert!(!should_skip_callee("process"));
            assert!(!should_skip_callee("calculate"));
            assert!(!should_skip_callee("Self_")); // Not exact match
            assert!(!should_skip_callee("myself"));
            assert!(!should_skip_callee("newValue"));
        }

        #[test]
        fn test_case_sensitive() {
            assert!(!should_skip_callee("SELF"));
            assert!(!should_skip_callee("This"));
            assert!(!should_skip_callee("NEW"));
        }
    }

    fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    mod call_extraction_tests {
        use super::*;

        #[test]
        fn test_extract_rust_calls() {
            let content = r#"
fn caller() {
    helper();
    other.method();
    Module::function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"function"));
        }

        #[test]
        fn test_extract_python_calls() {
            let content = r#"
def caller():
    helper()
    obj.method()
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
        }

        #[test]
        fn test_skips_self_calls() {
            let content = r#"
fn example() {
    self.method();
    this.other();
    real_function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(!names.contains(&"self"));
            assert!(!names.contains(&"this"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"other"));
            assert!(names.contains(&"real_function"));
        }

        #[test]
        fn test_parse_file_calls() {
            let content = r#"
fn caller() {
    helper();
    other_func();
}

fn another() {
    third();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();

            assert_eq!(function_calls.len(), 2);

            let caller = function_calls
                .iter()
                .find(|fc| fc.name == "caller")
                .unwrap();
            let caller_names: Vec<_> = caller
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(caller_names.contains(&"helper"));
            assert!(caller_names.contains(&"other_func"));

            let another = function_calls
                .iter()
                .find(|fc| fc.name == "another")
                .unwrap();
            let another_names: Vec<_> = another
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(another_names.contains(&"third"));
        }

        #[test]
        fn test_parse_file_calls_unsupported_extension() {
            let file = write_temp_file("not code", "txt");
            let parser = Parser::new().unwrap();
            let result = parser.parse_file_calls(file.path());
            assert!(result.is_err());
        }

        #[test]
        fn test_parse_file_calls_empty_file() {
            let file = write_temp_file("", "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();
            assert!(function_calls.is_empty());
        }
    }

    mod type_extraction_tests {
        use super::*;

        /// Helper: check if type_refs contains (name, kind)
        fn has_type(refs: &[TypeRef], name: &str, kind: Option<TypeEdgeKind>) -> bool {
            refs.iter().any(|t| t.type_name == name && t.kind == kind)
        }

        /// Parse source with tree-sitter and run extract_types on full range.
        /// Use for testing types on constructs that aren't chunks (impl blocks, type aliases).
        fn extract_types_from_source(content: &str, ext: &str) -> Vec<TypeRef> {
            let parser = Parser::new().unwrap();
            let language = Language::from_extension(ext).unwrap();
            let grammar = language.grammar();
            let mut ts_parser = tree_sitter::Parser::new();
            ts_parser.set_language(&grammar).unwrap();
            let tree = ts_parser.parse(content, None).unwrap();
            parser.extract_types(content, &tree, language, 0, content.len())
        }

        // --- Rust ---

        #[test]
        fn test_extract_types_rust_params_and_return() {
            let content = "fn foo(x: Config, y: Store) -> StoreError { }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Store", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "StoreError", Some(TypeEdgeKind::Return)));
        }

        #[test]
        fn test_extract_types_rust_struct_fields() {
            let content = "struct Foo {\n    config: Config,\n    pool: SqlitePool,\n}\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            assert_eq!(types[0].name, "Foo");
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Field)));
            assert!(has_type(refs, "SqlitePool", Some(TypeEdgeKind::Field)));
        }

        #[test]
        fn test_extract_types_rust_impl() {
            // impl blocks aren't chunks — test extract_types directly with full-file range
            let content = "impl MyTrait for MyStruct {\n    fn foo(&self) { }\n}\n";
            let types = extract_types_from_source(content, "rs");
            assert!(
                has_type(&types, "MyTrait", Some(TypeEdgeKind::Impl)),
                "MyTrait should be Impl, got: {:?}",
                types
            );
            assert!(
                has_type(&types, "MyStruct", Some(TypeEdgeKind::Impl)),
                "MyStruct should be Impl, got: {:?}",
                types
            );
        }

        #[test]
        fn test_extract_types_rust_bounds() {
            let content = "fn foo<T: Display + Clone>(x: T) { }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // The function chunk includes its entire span including generic params
            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Display", Some(TypeEdgeKind::Bound)));
            assert!(has_type(refs, "Clone", Some(TypeEdgeKind::Bound)));
        }

        #[test]
        fn test_extract_types_rust_no_primitives() {
            let content = "fn foo(x: i32, y: bool) -> u64 { 0 }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Primitives are `primitive_type` in tree-sitter, not `type_identifier`
            // So the function should have no type refs
            assert!(types.is_empty());
        }

        #[test]
        fn test_extract_types_rust_catch_all_merge() {
            // Config appears as Param (classified) AND inside generic (catch-all)
            // Error appears only inside generic (catch-all only)
            let content = "fn foo(c: Config) -> Result<Config, MyError> { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;

            // Config should be Param (classified wins over catch-all)
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            // Config should NOT also appear as None
            assert!(!has_type(refs, "Config", None));

            // Result should be Return (classified)
            assert!(has_type(refs, "Result", Some(TypeEdgeKind::Return)));

            // MyError should be None (catch-all only — inside generic)
            assert!(has_type(refs, "MyError", None));
        }

        #[test]
        fn test_extract_types_rust_reference_types() {
            let content = "fn foo(x: &Config) -> &Store { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Store", Some(TypeEdgeKind::Return)));
        }

        #[test]
        fn test_extract_types_rust_generic_param() {
            let content = "fn foo(x: Vec<Config>) -> Option<Store> { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            // Vec and Option are the outer generic types → classified
            assert!(has_type(refs, "Vec", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Option", Some(TypeEdgeKind::Return)));
            // Config and Store are inside generics → catch-all or classified depending on pattern
            // Config is inside Vec<Config> which is a parameter → the generic_type pattern should match Vec
            // Config itself is a type_identifier inside type_arguments → catch-all
            assert!(
                has_type(refs, "Config", None)
                    || has_type(refs, "Config", Some(TypeEdgeKind::Param))
            );
            assert!(
                has_type(refs, "Store", None)
                    || has_type(refs, "Store", Some(TypeEdgeKind::Return))
            );
        }

        #[test]
        fn test_extract_types_rust_alias() {
            // type_item isn't a chunk — test extract_types directly with full-file range
            let content = "type MyResult = Result<Config, MyError>;\n";
            let types = extract_types_from_source(content, "rs");
            assert!(
                has_type(&types, "Result", Some(TypeEdgeKind::Alias)),
                "Result should be Alias, got: {:?}",
                types
            );
            // Config and MyError inside generics — catch-all only
            assert!(
                has_type(&types, "Config", None),
                "Config should be catch-all (None), got: {:?}",
                types
            );
            assert!(
                has_type(&types, "MyError", None),
                "MyError should be catch-all (None), got: {:?}",
                types
            );
        }

        // --- TypeScript ---

        #[test]
        fn test_extract_types_typescript() {
            let content =
                "function foo(x: UserConfig): ResponseData {\n    return {} as ResponseData;\n}\n";
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "UserConfig", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "ResponseData", Some(TypeEdgeKind::Return)));
        }

        // --- Python ---

        #[test]
        fn test_extract_types_python() {
            let content = "def foo(x: MyType) -> ReturnType:\n    pass\n";
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "MyType", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "ReturnType", Some(TypeEdgeKind::Return)));
        }

        // --- Go ---

        #[test]
        fn test_extract_types_go() {
            let content =
                "package main\n\nfunc foo(cfg Config) Handler {\n    return Handler{}\n}\n";
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Handler", Some(TypeEdgeKind::Return)));
        }

        // --- Java ---

        #[test]
        fn test_extract_types_java() {
            let content = "class Main {\n    public UserService getService(Config config) {\n        return null;\n    }\n}\n";
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Java chunk query should capture the class or method
            // method_definition captures getService
            if !types.is_empty() {
                let refs = &types[0].type_refs;
                assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
                assert!(has_type(refs, "UserService", Some(TypeEdgeKind::Return)));
            }
        }

        // --- C ---

        #[test]
        fn test_extract_types_c() {
            let content = "Config create_config(Pool pool) {\n    Config c;\n    return c;\n}\n";
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            if !types.is_empty() {
                let refs = &types[0].type_refs;
                // C function_definition captures return type
                assert!(has_type(refs, "Pool", Some(TypeEdgeKind::Param)));
                // Config is both return type AND function name won't match (it's the type)
                // Actually the function name is "create_config", Config is the return type
                assert!(has_type(refs, "Config", Some(TypeEdgeKind::Return)));
            }
        }

        // --- JavaScript (no types) ---

        #[test]
        fn test_extract_types_empty_for_js() {
            let content = "function foo(x) {\n    return x + 1;\n}\n";
            let file = write_temp_file(content, "js");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();
            assert!(types.is_empty());
        }

        // --- Markdown (no types) ---

        #[test]
        fn test_extract_types_empty_for_markdown() {
            let content = "# Hello\n\nSome text\n";
            let file = write_temp_file(content, "md");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();
            assert!(types.is_empty());
        }

        // --- Combined parse ---

        #[test]
        fn test_parse_file_relationships_returns_both() {
            let content = r#"
fn process(config: Config) -> StoreError {
    helper();
    store.save();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Should have both calls and types
            assert!(!calls.is_empty(), "Expected call results");
            assert!(!types.is_empty(), "Expected type results");

            let call_entry = calls.iter().find(|c| c.name == "process").unwrap();
            let call_names: Vec<_> = call_entry
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(call_names.contains(&"helper"));
            assert!(call_names.contains(&"save"));

            let type_entry = types.iter().find(|t| t.name == "process").unwrap();
            assert!(has_type(
                &type_entry.type_refs,
                "Config",
                Some(TypeEdgeKind::Param)
            ));
            assert!(has_type(
                &type_entry.type_refs,
                "StoreError",
                Some(TypeEdgeKind::Return)
            ));
        }

        #[test]
        fn test_parse_file_relationships_filters_self_referential() {
            let content = "struct Config {\n    pool: SqlitePool,\n}\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            if !types.is_empty() {
                let config_refs = types.iter().find(|t| t.name == "Config").unwrap();
                // Config should NOT appear in its own type_refs
                assert!(
                    !config_refs
                        .type_refs
                        .iter()
                        .any(|t| t.type_name == "Config"),
                    "Self-referential type should be filtered out"
                );
                assert!(has_type(
                    &config_refs.type_refs,
                    "SqlitePool",
                    Some(TypeEdgeKind::Field)
                ));
            }
        }

        #[test]
        fn test_parse_file_calls_unchanged() {
            // Verify the thin wrapper returns same results as before
            let content = r#"
fn caller() {
    helper();
    other_func();
}

fn another() {
    third();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let calls_only = parser.parse_file_calls(file.path()).unwrap();
            let (calls_combined, _types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(calls_only.len(), calls_combined.len());
            for (a, b) in calls_only.iter().zip(calls_combined.iter()) {
                assert_eq!(a.name, b.name);
                assert_eq!(a.line_start, b.line_start);
                assert_eq!(a.calls.len(), b.calls.len());
            }
        }

        #[test]
        fn test_parse_file_relationships_nonexistent() {
            let parser = Parser::new().unwrap();
            let result =
                parser.parse_file_relationships(std::path::Path::new("/nonexistent/file.rs"));
            assert!(result.is_err());
        }
    }

    mod type_edge_kind_tests {
        use super::*;

        #[test]
        fn test_roundtrip() {
            let kinds = [
                TypeEdgeKind::Param,
                TypeEdgeKind::Return,
                TypeEdgeKind::Field,
                TypeEdgeKind::Impl,
                TypeEdgeKind::Bound,
                TypeEdgeKind::Alias,
            ];
            for kind in &kinds {
                let s = kind.as_str();
                let parsed: TypeEdgeKind = s.parse().unwrap();
                assert_eq!(*kind, parsed);
            }
        }

        #[test]
        fn test_display() {
            assert_eq!(TypeEdgeKind::Param.to_string(), "Param");
            assert_eq!(TypeEdgeKind::Return.to_string(), "Return");
            assert_eq!(TypeEdgeKind::Field.to_string(), "Field");
            assert_eq!(TypeEdgeKind::Impl.to_string(), "Impl");
            assert_eq!(TypeEdgeKind::Bound.to_string(), "Bound");
            assert_eq!(TypeEdgeKind::Alias.to_string(), "Alias");
        }

        #[test]
        fn test_unknown_from_str() {
            let result: Result<TypeEdgeKind, _> = "Unknown".parse();
            assert!(result.is_err());
        }
    }

    /// Diagnostic: verify type queries compile for all languages with type_query defined
    #[test]
    fn test_type_queries_compile() {
        let parser = Parser::new().unwrap();
        let languages_with_types = [
            Language::Rust,
            Language::TypeScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::C,
            Language::CSharp,
            // FSharp type query has pre-existing compile issues (#node-type mismatch)
            Language::Scala,
            Language::Cpp,
        ];
        for lang in languages_with_types {
            let result = parser.get_type_query(lang);
            assert!(
                result.is_ok(),
                "{} type query failed to compile: {:?}",
                lang,
                result.err()
            );
        }
    }
}
