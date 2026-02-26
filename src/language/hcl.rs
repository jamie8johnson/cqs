//! HCL/Terraform language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting HCL blocks.
///
/// All HCL blocks are generic `block` nodes. The `post_process_chunk` hook
/// determines the actual name and ChunkType based on the block's first identifier
/// (resource/variable/module/etc.) and string labels.
const CHUNK_QUERY: &str = r#"
;; All blocks — post_process_chunk determines name and type
(block
  (identifier) @name) @struct
"#;

/// Tree-sitter query for extracting HCL function calls
const CALL_QUERY: &str = r#"
;; HCL built-in function calls (lookup, format, toset, file, etc.)
(function_call
  (identifier) @callee)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "resource",
    "data",
    "variable",
    "output",
    "module",
    "provider",
    "terraform",
    "locals",
    "backend",
    "required_providers",
    "required_version",
    "count",
    "for_each",
    "depends_on",
    "lifecycle",
    "provisioner",
    "connection",
    "source",
    "version",
    "type",
    "default",
    "description",
    "sensitive",
    "validation",
    "condition",
    "error_message",
    "true",
    "false",
    "null",
    "each",
    "self",
    "var",
    "local",
    "path",
];

/// Post-process HCL blocks to determine correct name and ChunkType.
///
/// HCL's tree-sitter grammar represents all blocks as generic `block` nodes.
/// This hook walks the block's children to extract the block type (first identifier)
/// and string labels, then assigns the correct ChunkType and qualified name.
///
/// Filters out:
/// - Nested blocks (provisioner/lifecycle inside resources)
/// - Blocks with no labels (locals, terraform, required_providers)
fn post_process_hcl(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let _span = tracing::debug_span!("post_process_hcl", name = %name).entered();

    // Filter nested blocks: if parent is a body whose parent is another block, skip.
    // This prevents provisioner/lifecycle/connection inside resources from becoming chunks.
    if let Some(parent) = node.parent() {
        if parent.kind() == "body" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "block" {
                    tracing::debug!("Skipping nested block inside parent block");
                    return false;
                }
            }
        }
    }

    let mut block_type = None;
    let mut labels: Vec<String> = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if block_type.is_none() => {
                block_type = Some(source[child.byte_range()].to_string());
            }
            "string_lit" => {
                // Extract template_literal content (quote-free)
                let mut inner = child.walk();
                let mut found = false;
                for c in child.children(&mut inner) {
                    if c.kind() == "template_literal" {
                        labels.push(source[c.byte_range()].to_string());
                        found = true;
                    }
                }
                if !found {
                    // string_lit with no template_literal (empty string or interpolation-only)
                    tracing::trace!("string_lit with no template_literal child, skipping label");
                }
            }
            _ => {}
        }
    }

    let bt = block_type.as_deref().unwrap_or("");

    // Skip blocks with no labels (locals, terraform, required_providers)
    if labels.is_empty() {
        tracing::debug!(block_type = bt, "Skipping block with no labels");
        return false;
    }

    // Safe label access — guaranteed non-empty after check above
    let last_label = &labels[labels.len() - 1];

    match bt {
        "resource" | "data" => {
            *chunk_type = ChunkType::Struct;
            *name = if labels.len() >= 2 {
                format!("{}.{}", labels[0], labels[1])
            } else {
                last_label.clone()
            };
        }
        "variable" | "output" => {
            *chunk_type = ChunkType::Constant;
            *name = last_label.clone();
        }
        "module" => {
            *chunk_type = ChunkType::Module;
            *name = last_label.clone();
        }
        _ => {
            // provider, backend, unknown block types → Struct
            *chunk_type = ChunkType::Struct;
            *name = last_label.clone();
        }
    }

    tracing::debug!(
        block_type = bt,
        name = %name,
        chunk_type = ?chunk_type,
        "Reclassified HCL block"
    );
    true
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "hcl",
    grammar: Some(|| tree_sitter_hcl::LANGUAGE.into()),
    extensions: &["tf", "tfvars", "hcl"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_hcl),
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

    #[test]
    fn parse_hcl_resource() {
        let content = r#"
resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t2.micro"
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "aws_instance.web");
        assert_eq!(chunks[0].chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_hcl_data() {
        let content = r#"
data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "aws_ami.ubuntu");
        assert_eq!(chunks[0].chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_hcl_variable() {
        let content = r#"
variable "name" {
  type    = string
  default = "world"
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "name");
        assert_eq!(chunks[0].chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_hcl_output() {
        let content = r#"
output "vpc_id" {
  value = aws_vpc.main.id
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "vpc_id");
        assert_eq!(chunks[0].chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_hcl_module() {
        let content = r#"
module "vpc" {
  source = "./modules/vpc"
  cidr   = "10.0.0.0/16"
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "vpc");
        assert_eq!(chunks[0].chunk_type, ChunkType::Module);
    }

    #[test]
    fn parse_hcl_locals_skipped() {
        let content = r#"
locals {
  common_tags = {
    Environment = "dev"
  }
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert!(
            chunks.is_empty(),
            "locals block should be filtered out, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_hcl_calls() {
        let content = r#"
variable "tags" {
  default = lookup(var.base_tags, "env", "dev")
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let var = chunks.iter().find(|c| c.name == "tags").unwrap();
        let calls = parser.extract_calls_from_chunk(var);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"lookup"),
            "Expected lookup call, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_hcl_provider() {
        let content = r#"
provider "aws" {
  region = "us-east-1"
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "aws");
        assert_eq!(chunks[0].chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_hcl_empty_body() {
        let content = r#"
variable "x" {}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "x");
        assert_eq!(chunks[0].chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_hcl_nested_blocks() {
        let content = r#"
resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t2.micro"

  provisioner "local-exec" {
    command = "echo hello"
  }

  lifecycle {
    create_before_destroy = true
  }
}
"#;
        let file = write_temp_file(content, "tf");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Only the top-level resource should be captured, not provisioner or lifecycle
        assert_eq!(
            chunks.len(),
            1,
            "Expected only resource, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert_eq!(chunks[0].name, "aws_instance.web");
    }
}
