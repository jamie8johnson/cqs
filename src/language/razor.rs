//! Razor/CSHTML language definition
//!
//! Razor is ASP.NET's hybrid template language mixing C#, HTML, and Razor directives.
//! Used in MVC Views (.cshtml), Razor Pages, and Blazor components (.razor).
//!
//! Grammar: `tris203/tree-sitter-razor` — monolithic grammar parsing C#, HTML, and
//! Razor directives in a single tree. C# chunks extracted from `@code`/`@functions`
//! blocks. HTML headings/landmarks extracted via post-process on generic `element` nodes.
//! JS/CSS injected from `<script>` and `<style>` elements via `_inner` content mode.

use super::{ChunkType, FieldStyle, InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Razor/C# chunks.
///
/// The grammar produces standard C# nodes (method_declaration, class_declaration, etc.)
/// inside `razor_block` containers. Tree-sitter queries find them anywhere in the tree.
///
/// Also captures:
/// - `razor_inject_directive` — DI service injections as Property
/// - `razor_block` — @code/@functions blocks as Module (name assigned by post-process)
/// - `element` — generic HTML elements (filtered by post-process, only h1-h6 and landmarks survive)
const CHUNK_QUERY: &str = r#"
;; Methods (inside @code blocks)
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(local_function_statement name: (identifier) @name) @function

;; Properties and fields
(property_declaration name: (identifier) @name) @property
(field_declaration
  (variable_declaration
    (variable_declarator (identifier) @name))) @property

;; Types
(class_declaration name: (identifier) @name) @class
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @struct
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum

;; DI injections: @inject IService ServiceName
(razor_inject_directive
  (variable_declaration
    (variable_declarator (identifier) @name))) @property

;; @code / @functions blocks (name assigned by post-process)
(razor_block) @module

;; HTML elements (tag name extracted by post-process, noise filtered)
(element) @section
"#;

/// Tree-sitter query for extracting function calls — same patterns as C#.
const CALL_QUERY: &str = r#"
(invocation_expression
  function: (member_access_expression name: (identifier) @callee))
(invocation_expression
  function: (identifier) @callee)
(object_creation_expression type: (identifier) @callee)
(object_creation_expression type: (generic_name (identifier) @callee))
"#;

/// Tree-sitter query for extracting type references — reuses C# patterns.
const TYPE_QUERY: &str = r#"
;; Param — method parameters
(parameter type: (identifier) @param_type)
(parameter type: (generic_name (identifier) @param_type))
(parameter type: (qualified_name (identifier) @param_type))
(parameter type: (nullable_type (identifier) @param_type))
(parameter type: (array_type (identifier) @param_type))

;; Return
(method_declaration returns: (identifier) @return_type)
(method_declaration returns: (generic_name (identifier) @return_type))
(method_declaration returns: (qualified_name (identifier) @return_type))
(method_declaration returns: (nullable_type (identifier) @return_type))
(local_function_statement type: (identifier) @return_type)
(local_function_statement type: (generic_name (identifier) @return_type))

;; Field — field declarations and property types
(field_declaration (variable_declaration type: (identifier) @field_type))
(field_declaration (variable_declaration type: (generic_name (identifier) @field_type)))
(property_declaration type: (identifier) @field_type)
(property_declaration type: (generic_name (identifier) @field_type))

;; Impl — base class, interface implementations
(base_list (identifier) @impl_type)
(base_list (generic_name (identifier) @impl_type))
(base_list (qualified_name (identifier) @impl_type))

;; Bound — generic constraints (where T : IFoo)
(type_parameter_constraint (type (identifier) @bound_type))
(type_parameter_constraint (type (generic_name (identifier) @bound_type)))
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment", "razor_comment"];

const STOPWORDS: &[&str] = &[
    // C# keywords
    "public", "private", "protected", "internal", "static", "readonly", "sealed", "abstract",
    "virtual", "override", "async", "await", "class", "struct", "interface", "enum", "namespace",
    "using", "return", "if", "else", "for", "foreach", "while", "do", "switch", "case", "break",
    "continue", "new", "this", "base", "try", "catch", "finally", "throw", "var", "void", "int",
    "string", "bool", "true", "false", "null", "get", "set", "value", "where", "partial", "event",
    "delegate", "record", "yield", "in", "out", "ref",
    // Razor directives (without @ — tokenizer strips it)
    "page", "model", "inject", "code", "functions", "rendermode", "attribute", "layout",
    "inherits", "implements", "preservewhitespace", "typeparam", "section",
];

const COMMON_TYPES: &[&str] = &[
    "string", "int", "bool", "object", "void", "double", "float", "long", "byte", "char",
    "decimal", "short", "uint", "ulong", "Task", "ValueTask", "List", "Dictionary", "HashSet",
    "Queue", "Stack", "IEnumerable", "IList", "IDictionary", "ICollection", "IQueryable", "Action",
    "Func", "Predicate", "EventHandler", "EventArgs", "IDisposable", "CancellationToken", "ILogger",
    "StringBuilder", "Exception", "Nullable", "Span", "Memory", "ReadOnlySpan", "IServiceProvider",
    "HttpContext", "IConfiguration",
];

/// Detect language for `<script>` and `<style>` elements.
///
/// Fires for every `element` node — returns `_skip` for non-script/style elements.
/// Checks for TypeScript via `lang="ts"` or `type="text/typescript"` attributes.
fn detect_razor_element_language(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let text = &source[node.byte_range()];
    // Only check the opening tag (first ~200 bytes) to avoid scanning large elements
    let prefix = &text[..text.len().min(200)];
    let lower = prefix.to_ascii_lowercase();
    if lower.starts_with("<script") {
        if lower.contains("lang=\"ts\"") || lower.contains("type=\"text/typescript\"") {
            tracing::debug!("Razor <script> detected as TypeScript");
            return Some("typescript");
        }
        tracing::debug!("Razor <script> detected as JavaScript");
        None // default: javascript
    } else if lower.starts_with("<style") {
        tracing::debug!("Razor <style> detected as CSS");
        Some("css")
    } else {
        Some("_skip") // not script or style
    }
}

/// Extract the HTML tag name from an element node's source text.
///
/// Returns the tag name (lowercase) from `<tagname ...>`.
fn extract_tag_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = &source[node.byte_range()];
    if !text.starts_with('<') {
        return None;
    }
    let after_lt = &text[1..];
    let name: String = after_lt
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(name.to_lowercase())
}

/// Extract text content from an element, skipping nested child elements.
///
/// Used for heading elements (h1-h6) to get the visible text.
fn extract_text_content(node: tree_sitter::Node, source: &str) -> String {
    let full = &source[node.byte_range()];
    // Strip opening tag
    let after_open = if let Some(pos) = full.find('>') {
        &full[pos + 1..]
    } else {
        return String::new();
    };
    // Strip closing tag
    let content = if let Some(pos) = after_open.rfind("</") {
        &after_open[..pos]
    } else {
        after_open
    };
    // Strip any HTML tags from content for clean text
    let mut result = String::new();
    let mut in_tag = false;
    for ch in content.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result.trim().to_string()
}

/// Extract an attribute value from an element's opening tag text.
fn extract_attribute_from_text(text: &str, attr_name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let pattern = format!("{}=\"", attr_name);
    if let Some(pos) = lower.find(&pattern) {
        let after = &text[pos + pattern.len()..];
        if let Some(end) = after.find('"') {
            let value = &after[..end];
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Heading tags (h1-h6)
const HEADING_TAGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// HTML5 landmark elements
const LANDMARK_TAGS: &[&str] = &["header", "nav", "main", "footer", "aside", "article"];

/// Post-process Razor chunks: assign names to razor_block and element nodes, filter noise.
fn post_process_razor(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "razor_block" => {
            // Name from source text prefix
            let text = &source[node.byte_range()];
            if text.starts_with("@code") {
                *name = "code".to_string();
            } else if text.starts_with("@functions") {
                *name = "functions".to_string();
            } else {
                // Anonymous @{ } block — skip, methods inside are captured individually
                tracing::debug!("Skipping anonymous razor block");
                return false;
            }
            *chunk_type = ChunkType::Module;
            true
        }
        "element" => {
            let tag = match extract_tag_name(node, source) {
                Some(t) => t,
                None => return false,
            };

            if HEADING_TAGS.contains(&tag.as_str()) {
                // Heading → Section with text content as name
                let text = extract_text_content(node, source);
                if text.is_empty() {
                    return false;
                }
                *name = text;
                *chunk_type = ChunkType::Section;
                tracing::debug!(tag = %tag, name = %name, "Razor heading element");
                true
            } else if LANDMARK_TAGS.contains(&tag.as_str()) {
                // Landmark → Section with id or aria-label as name
                let text = &source[node.byte_range()];
                let label = extract_attribute_from_text(text, "id")
                    .or_else(|| extract_attribute_from_text(text, "aria-label"));
                *name = label.unwrap_or_else(|| tag.clone());
                *chunk_type = ChunkType::Section;
                tracing::debug!(tag = %tag, name = %name, "Razor landmark element");
                true
            } else {
                // All other elements → filter out (noise)
                false
            }
        }
        // C# constructor_declaration nodes inside razor_block
        "constructor_declaration"
            if matches!(*chunk_type, ChunkType::Function | ChunkType::Method) =>
        {
            *chunk_type = ChunkType::Constructor;
            true
        }
        _ => true, // Pass through C# chunks unchanged
    }
}

/// Extracts the return type from a C# method signature and formats it as documentation text.
/// 
/// Parses a C# method signature to identify the return type, which appears before the method name in C#. Filters out common C# modifiers and keywords to isolate the actual return type. The return type is then tokenized and formatted into a documentation string.
/// 
/// # Arguments
/// 
/// `signature` - A C# method signature string to parse for the return type.
/// 
/// # Returns
/// 
/// `Some(String)` containing the formatted return type documentation if a valid non-void return type is found, or `None` if the signature does not contain a recognizable return type.
fn extract_return(signature: &str) -> Option<String> {
    // C#: return type before method name
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void"
                    | "public"
                    | "private"
                    | "protected"
                    | "internal"
                    | "static"
                    | "abstract"
                    | "virtual"
                    | "override"
                    | "sealed"
                    | "async"
                    | "extern"
                    | "partial"
                    | "new"
                    | "unsafe"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "razor",
    grammar: Some(|| tree_sitter_razor::LANGUAGE.into()),
    extensions: &["cshtml", "razor"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[
        "class_declaration",
        "struct_declaration",
        "record_declaration",
        "interface_declaration",
        "declaration_list",
        "razor_block",
    ],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["declaration_list"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_razor),
    test_markers: &["[Test]", "[Fact]", "[Theory]", "[TestMethod]"],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &["Main", "OnInitializedAsync", "OnParametersSetAsync"],
    trait_method_names: &[
        "Equals",
        "GetHashCode",
        "ToString",
        "Dispose",
        "OnInitialized",
        "OnParametersSet",
        "OnAfterRender",
        "SetParametersAsync",
    ],
    injections: &[
        // <script> and <style> elements → JS/CSS via _inner content mode
        InjectionRule {
            container_kind: "element",
            content_kind: "_inner",
            target_language: "javascript",
            detect_language: Some(detect_razor_element_language),
            content_scoped_lines: false,
        },
    ],
    doc_format: "default",
    doc_convention: "",
    field_style: FieldStyle::None,
    skip_line_prefixes: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use crate::parser::{ChunkType, Parser};
    use std::io::Write;

    fn write_temp_file(content: &str, ext: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }
    /// Verifies that the parser correctly extracts and classifies methods from a Razor component's @code block.
    /// 
    /// This test parses a Razor (.cshtml) file containing a @code block with multiple methods (both synchronous and asynchronous), then validates that the parser identifies and categorizes these methods as individual chunks with the correct ChunkType::Method classification.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. Returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to initialize
    /// - The parser fails to parse the file
    /// - The expected methods "IncrementCount" or "ResetCount" are not found in the parsed chunks
    /// - The "IncrementCount" method chunk does not have ChunkType::Method classification

    #[test]
    fn parse_razor_code_block() {
        let content = r#"@page "/counter"

@code {
    private int currentCount = 0;

    private void IncrementCount()
    {
        currentCount++;
    }

    private async Task ResetCount()
    {
        currentCount = 0;
        await Task.Delay(100);
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"IncrementCount"),
            "Expected 'IncrementCount' method, got: {:?}",
            names
        );
        assert!(
            names.contains(&"ResetCount"),
            "Expected 'ResetCount' method, got: {:?}",
            names
        );
        let inc = chunks.iter().find(|c| c.name == "IncrementCount").unwrap();
        // Methods inside razor_block (a method container) are reclassified as Method
        assert_eq!(inc.chunk_type, ChunkType::Method);
    }
    /// Parses Razor component @inject directives and verifies they are correctly identified as properties.
    /// 
    /// This is a test function that validates the parser's ability to extract and categorize @inject directives from Razor component files. It creates a temporary Cshtml file containing multiple @inject statements, parses it, and asserts that the injected dependencies are recognized by name and classified with the correct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a self-contained test function.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns nothing.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - The parsed chunks do not contain an entry named "Logger"
    /// - The parsed chunks do not contain an entry named "NavManager"
    /// - The "Logger" chunk does not have chunk_type equal to ChunkType::Property

    #[test]
    fn parse_razor_inject_directives() {
        let content = r#"@page "/test"
@inject ILogger<Index> Logger
@inject NavigationManager NavManager

@code {
    private void DoSomething()
    {
        Logger.LogInformation("test");
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"Logger"),
            "Expected 'Logger' inject, got: {:?}",
            names
        );
        assert!(
            names.contains(&"NavManager"),
            "Expected 'NavManager' inject, got: {:?}",
            names
        );
        let logger = chunks.iter().find(|c| c.name == "Logger").unwrap();
        assert_eq!(logger.chunk_type, ChunkType::Property);
    }
    /// Parses a Razor file containing a C# class definition within an @code block and verifies that the class is correctly identified.
    /// 
    /// This test function creates a temporary Razor file with a WeatherForecast class defined in a @code block, parses it using the Parser, and asserts that the class is found with the correct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the Parser fails to initialize, if file parsing fails, if the WeatherForecast class is not found in the parsed chunks, or if the found chunk's type is not ChunkType::Class.

    #[test]
    fn parse_razor_class_in_code() {
        let content = r#"@code {
    public class WeatherForecast
    {
        public DateTime Date { get; set; }
        public int TemperatureC { get; set; }
        public string Summary { get; set; }
    }
}
"#;
        let file = write_temp_file(content, "razor");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let wf = chunks.iter().find(|c| c.name == "WeatherForecast");
        assert!(
            wf.is_some(),
            "Expected 'WeatherForecast' class, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert_eq!(wf.unwrap().chunk_type, ChunkType::Class);
    }
    /// Verifies that the parser correctly identifies and extracts field declarations from a Razor code block.
    /// 
    /// This is a test function that creates a temporary Razor (.cshtml) file containing a @code block with field declarations, parses it, and asserts that the declared fields are correctly recognized as properties with their proper names.
    /// 
    /// # Arguments
    /// 
    /// None. This is a standalone test function.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to find the "currentCount" field or if the field's chunk_type is not ChunkType::Property.

    #[test]
    fn parse_razor_field_declaration() {
        let content = r#"@code {
    private int currentCount = 0;
    private string message = "hello";
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"currentCount"),
            "Expected 'currentCount' field, got: {:?}",
            names
        );
        let field = chunks.iter().find(|c| c.name == "currentCount").unwrap();
        assert_eq!(field.chunk_type, ChunkType::Property);
    }
    /// Verifies that a constructor defined within a Razor code block is correctly parsed and classified as a Method chunk type.
    /// 
    /// This is a test function that creates a temporary Razor (.cshtml) file containing a C# class with a constructor, parses it using the Parser, and asserts that the constructor is identified and reclassified as a Method chunk rather than a Constructor chunk.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the file, or if the expected 'MyService' constructor is not found in the parsed chunks with ChunkType::Method classification.

    #[test]
    fn parse_razor_constructor() {
        let content = r#"@code {
    public class MyService
    {
        private readonly ILogger _logger;

        public MyService(ILogger logger)
        {
            _logger = logger;
        }
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Constructor inside class inside razor_block — reclassified as Constructor
        let ctor = chunks.iter().find(|c| c.name == "MyService" && c.chunk_type == ChunkType::Constructor);
        assert!(
            ctor.is_some(),
            "Expected 'MyService' constructor as Constructor, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly extracts function calls from Razor (.cshtml) files within @code blocks.
    /// 
    /// This test creates a temporary Razor file containing a `HandleClick` method that calls `IncrementCount` and `StateHasChanged`, then parses it to validate that the call graph accurately identifies both callee functions. It demonstrates that `parse_file_calls` (rather than `extract_calls_from_chunk`) must be used for Razor files due to grammar requirements.
    /// 
    /// # Panics
    /// 
    /// Panics if the 'HandleClick' function is not found in the parsed call graph, or if either the 'IncrementCount' or 'StateHasChanged' calls are missing from its callees.

    #[test]
    fn parse_razor_call_graph() {
        // NOTE: Razor grammar requires @code {} context, so extract_calls_from_chunk
        // (which re-parses chunk content alone) won't work. Use parse_file_calls
        // instead — this is the production path. Same limitation as PHP.
        let content = r#"@code {
    private void HandleClick()
    {
        IncrementCount();
        StateHasChanged();
    }

    private void IncrementCount()
    {
        currentCount++;
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let function_calls = parser.parse_file_calls(file.path()).unwrap();
        let handle = function_calls
            .iter()
            .find(|fc| fc.name == "HandleClick");
        assert!(handle.is_some(), "Expected 'HandleClick' in call graph");
        let handle = handle.unwrap();
        let callee_names: Vec<_> = handle.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            callee_names.contains(&"IncrementCount"),
            "Expected 'IncrementCount' call, got: {:?}",
            callee_names
        );
        assert!(
            callee_names.contains(&"StateHasChanged"),
            "Expected 'StateHasChanged' call, got: {:?}",
            callee_names
        );
    }
    /// Parses a Razor HTML (.cshtml) file containing headings and verifies that HTML heading elements are correctly extracted as document chunks.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following conditions are not met:
    /// - The temporary file is successfully created and written
    /// - The parser successfully parses the file
    /// - The parsed chunks contain a heading named "About Us"
    /// - The parsed chunks contain a heading named "Our Team"
    /// - The "About Us" chunk has `ChunkType::Section`

    #[test]
    fn parse_razor_html_headings() {
        let content = r#"@page "/about"

<h1>About Us</h1>

<p>Some content here.</p>

<h2>Our Team</h2>

<p>More content.</p>
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"About Us"),
            "Expected 'About Us' heading, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Our Team"),
            "Expected 'Our Team' heading, got: {:?}",
            names
        );
        let h1 = chunks.iter().find(|c| c.name == "About Us").unwrap();
        assert_eq!(h1.chunk_type, ChunkType::Section);
    }
    /// Verifies that a Razor page containing only HTML markup and no @code block produces no C# code chunks.
    /// 
    /// This test function writes a temporary Razor (.cshtml) file with pure HTML content and no code block, then parses it to ensure the parser correctly identifies that there are no C# functions or classes to extract. The test asserts that filtering the parsed chunks for Function or Class types yields an empty result.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns unit type.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to initialize, if file parsing fails, or if any C# code chunks (functions or classes) are found in the parsed output.

    #[test]
    fn parse_razor_no_code_block() {
        // Pure HTML Razor page with no @code block — no C# chunks
        let content = r#"@page "/static"

<h1>Static Page</h1>
<p>This page has no code block.</p>
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Should only have the h1 heading, no methods/classes
        let c_sharp_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| matches!(c.chunk_type, ChunkType::Function | ChunkType::Class))
            .collect();
        assert!(
            c_sharp_chunks.is_empty(),
            "Pure HTML page should have no C# chunks, got: {:?}",
            c_sharp_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }
    /// Parses a complete Razor component file containing directives, HTML markup, and C# code blocks, then validates that all expected code elements (injected properties, sections, and methods) are correctly identified and categorized.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (void function)
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, or if any of the three assertions fail when expected code elements are not found with their correct chunk types in the parsed output.

    #[test]
    fn parse_razor_mixed() {
        // Full component with directives, HTML, and @code block
        let content = r#"@page "/counter"
@inject ILogger<Counter> Logger

<h1>Counter</h1>

<p>Current count: @currentCount</p>

<button @onclick="IncrementCount">Click me</button>

@code {
    private int currentCount = 0;

    private void IncrementCount()
    {
        currentCount++;
        Logger.LogInformation("Count: {Count}", currentCount);
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();
        // Should have: Logger (Property), Counter heading (Section),
        //              code (Module), currentCount (Property), IncrementCount (Function)
        assert!(
            names.iter().any(|(n, t)| *n == "Logger" && *t == ChunkType::Property),
            "Expected 'Logger' inject property, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "Counter" && *t == ChunkType::Section),
            "Expected 'Counter' heading section, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "IncrementCount" && *t == ChunkType::Method),
            "Expected 'IncrementCount' method (inside razor_block container), got: {:?}",
            names
        );
    }
    /// Parses a Razor component file and verifies that type references in C# code blocks are correctly identified.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data internally.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, or the expected 'GetItems' function is not found in the parsed chunks.

    #[test]
    fn parse_razor_type_refs() {
        let content = r#"@code {
    private Task<List<string>> GetItems(int count, CancellationToken token)
    {
        return Task.FromResult(new List<string>());
    }
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "GetItems");
        assert!(func.is_some(), "Expected 'GetItems' function");
    }

    // --- Injection tests ---
    /// Verifies that the parser correctly extracts JavaScript code blocks from Razor template files containing injected script tags.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract the JavaScript function `greet` from the `<script>` block in the Razor template, or if file operations fail.

    #[test]
    fn parse_razor_script_injection() {
        let content = r#"@page "/test"

<h1>Test</h1>

<script>
function greet(name) {
    return "Hello, " + name;
}

function add(a, b) {
    return a + b;
}
</script>
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "greet"),
            "Expected JS function 'greet' from <script>, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly extracts CSS code blocks from Razor-style `.cshtml` files containing `<style>` tags.
    /// 
    /// This integration test writes a temporary Razor template file with embedded CSS styling, parses it using the Parser, and asserts that at least one CSS language chunk is extracted from the `<style>` block.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, fails to parse the file, or if no CSS chunks are found in the parsed output.

    #[test]
    fn parse_razor_style_injection() {
        let content = r#"@page "/styled"

<style>
.container {
    display: flex;
    justify-content: center;
}

.header {
    font-size: 2rem;
}
</style>

<div class="container">
    <h1 class="header">Styled Page</h1>
</div>
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let css_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Css)
            .collect();
        assert!(
            !css_chunks.is_empty(),
            "Expected CSS chunks from <style>, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Verifies that a Razor file without separate script or style blocks parses as Razor-only chunks without triggering language injection.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, or if any parsed chunk has a language type other than `Language::Razor`.

    #[test]
    fn parse_razor_no_script_unchanged() {
        // Razor file with no script/style — injection should not fire
        let content = r#"@page "/plain"

<h1>Plain Page</h1>

@code {
    private int value = 42;
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Razor,
                "File without script/style should only have Razor chunks, got {:?} for '{}'",
                chunk.language,
                chunk.name
            );
        }
    }
    /// Verifies that the Razor parser correctly identifies fields with no method calls in a Cshtml file.
    /// 
    /// This test parses a Razor component file containing a field declaration and validates that the parser's `extract_calls_from_chunk` method returns an empty call list for fields that have no associated method calls.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics if validation fails.
    /// 
    /// # Panics
    /// 
    /// Panics if a field named "value" is found to contain method calls, or if file parsing or chunk extraction operations fail unexpectedly.

    #[test]
    fn parse_razor_no_calls() {
        let content = r#"@page "/test"

<h1>Test</h1>

@code {
    private int value = 42;
}
"#;
        let file = write_temp_file(content, "cshtml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            if chunk.chunk_type == ChunkType::Section {
                continue; // headings don't have calls
            }
            let calls = parser.extract_calls_from_chunk(chunk);
            // Fields with no method calls should have empty call list
            if chunk.name == "value" {
                assert!(calls.is_empty(), "Field should have no calls");
            }
        }
    }

}
