//! Call extraction from tree-sitter parse trees

use std::path::Path;
use tree_sitter::StreamingIterator;

use super::types::{CallSite, FunctionCalls, Language, ParserError};
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
        self.extract_calls(
            &chunk.content,
            chunk.language,
            0,
            chunk.content.len(),
            0, // No line offset since we're parsing the content directly
        )
    }

    /// Extract all function calls from a file, ignoring size limits
    ///
    /// Returns calls for every function in the file, including those >100 lines
    /// that would normally be skipped during chunk extraction.
    pub fn parse_file_calls(&self, path: &Path) -> Result<Vec<FunctionCalls>, ParserError> {
        // Check file size (matching parse_file limit)
        const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                tracing::warn!(
                    "Skipping large file ({}MB > 50MB limit): {}",
                    meta.len() / (1024 * 1024),
                    path.display()
                );
                return Ok(vec![]);
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(vec![]);
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistency
        let source = source.replace("\r\n", "\n");

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Language::from_extension(ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

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

        let mut results = Vec::new();
        // Reuse these allocations across iterations
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();

        while let Some(m) = matches.next() {
            // Find function node
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                matches!(
                    name,
                    "function" | "struct" | "class" | "enum" | "trait" | "interface" | "const"
                )
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get function name
            let name_idx = chunk_query.capture_index_for_name("name");
            let name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            let line_start = node.start_position().row as u32 + 1;

            // Extract calls within this function (no size limit!)
            call_cursor.set_byte_range(node.byte_range());
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

            // Deduplicate
            seen.clear();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                results.push(FunctionCalls {
                    name,
                    line_start,
                    calls: std::mem::take(&mut calls),
                });
            }
        }

        Ok(results)
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
}
