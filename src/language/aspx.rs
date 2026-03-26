//! ASP.NET Web Forms language definition
//!
//! Web Forms files (.aspx, .ascx, .asmx, .master) contain HTML with embedded
//! C# or VB.NET server-side code. No tree-sitter grammar — uses a custom parser
//! that delegates to C#/VB.NET grammars via `set_included_ranges()`.

use super::{FieldStyle, LanguageDef, SignatureStyle};

const STOPWORDS: &[&str] = &[
    "page", "control", "master", "runat", "server", "autopostback", "viewstate",
    "postback", "handler", "event", "sender", "eventargs", "codebehind",
    "inherits", "aspx", "ascx", "asmx",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "aspx",
    grammar: None, // Custom parser — delegates to C#/VB.NET grammars
    extensions: &["aspx", "ascx", "asmx", "master"],
    chunk_query: "",
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &[],
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &["Page_Load", "Page_Init", "Page_PreRender"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "default",
    doc_convention: "",
    field_style: FieldStyle::None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
