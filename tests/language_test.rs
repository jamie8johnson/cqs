//! Language parsing tests -- consolidated from per-language #[cfg(test)] blocks.

use cqs::parser::{ChunkType, Language, Parser};
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
// -- bash ────────────────────────────────────────────────────────────

#[test]
fn parse_bash_function() {
    let content = r#"
function foo() {
    echo "hello"
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].name, "foo");
    assert_eq!(chunks[0].chunk_type, ChunkType::Function);
}

#[test]
fn parse_bash_function_short() {
    let content = r#"
foo() {
    echo "hello"
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].name, "foo");
    assert_eq!(chunks[0].chunk_type, ChunkType::Function);
}

#[test]
fn parse_bash_calls() {
    let content = r#"
function deploy() {
    echo "deploying..."
    grep -r "TODO" src/
    run_tests
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "deploy").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"echo"), "Expected echo, got: {:?}", names);
    assert!(names.contains(&"grep"), "Expected grep, got: {:?}", names);
    assert!(
        names.contains(&"run_tests"),
        "Expected run_tests, got: {:?}",
        names
    );
}

#[test]
fn parse_bash_multiline() {
    let content = r#"
function setup_env() {
    local env_name="$1"
    if [ -z "$env_name" ]; then
        echo "Usage: setup_env <name>"
        return 1
    fi
    export ENV_NAME="$env_name"
    echo "Environment set to $env_name"
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].name, "setup_env");
    assert!(chunks[0].content.contains("local env_name"));
}

#[test]
fn parse_bash_nested_calls() {
    let content = r#"
function build() {
    compile_sources
    run_tests
    package_artifacts
}

function compile_sources() {
    gcc -o main main.c
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert_eq!(chunks.len(), 2);
    let build = chunks.iter().find(|c| c.name == "build").unwrap();
    let calls = parser.extract_calls_from_chunk(build);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"compile_sources"),
        "Expected compile_sources, got: {:?}",
        names
    );
    assert!(
        names.contains(&"run_tests"),
        "Expected run_tests, got: {:?}",
        names
    );
}

#[test]
fn parse_bash_no_chunks_outside_function() {
    let content = r#"
#!/bin/bash
echo "standalone command"
ls -la
# This is a comment
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks.is_empty(),
        "Expected no chunks for bare commands, got: {:?}",
        chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_bash_readonly_constant() {
    let content = r#"
readonly MAX_RETRIES=3
readonly API_URL="https://example.com"
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(
        max.is_some(),
        "Should capture MAX_RETRIES, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
    let url = chunks.iter().find(|c| c.name == "API_URL");
    assert!(url.is_some(), "Should capture API_URL");
    assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
}

#[test]
fn parse_bash_doc_comment() {
    let content = r#"
# Deploy the application to production
function deploy() {
    echo "deploying"
}
"#;
    let file = write_temp_file(content, "sh");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "deploy").unwrap();
    assert!(
        func.doc.as_ref().map_or(false, |d| d.contains("Deploy")),
        "Expected doc comment, got: {:?}",
        func.doc
    );
}

// -- c ───────────────────────────────────────────────────────────────

#[test]
fn parse_c_union() {
    let content = "union Data {\n  int i;\n  float f;\n};\n";
    let file = write_temp_file(content, "c");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let u = chunks.iter().find(|c| c.name == "Data").unwrap();
    assert_eq!(u.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_c_define_constant() {
    let content = "#define MAX_SIZE 1024\n";
    let file = write_temp_file(content, "c");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let c = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
    assert_eq!(c.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_c_define_macro() {
    let content = "#define SWAP(a, b) do { int t = a; a = b; b = t; } while(0)\n";
    let file = write_temp_file(content, "c");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let m = chunks.iter().find(|c| c.name == "SWAP").unwrap();
    assert_eq!(m.chunk_type, ChunkType::Macro);
}

#[test]
fn parse_c_typedef_as_typealias() {
    let content = "typedef int MyInt;\n";
    let file = write_temp_file(content, "c");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

// -- cpp ─────────────────────────────────────────────────────────────

#[test]
fn parse_cpp_free_function() {
    let content = "void foo() {\n  // body\n}\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "foo").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
    assert!(func.parent_type_name.is_none());
}

#[test]
fn parse_cpp_class() {
    let content = r#"
class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_cpp_struct() {
    let content = "struct Point {\n  double x;\n  double y;\n};\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Point").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_cpp_namespace() {
    let content = r#"
namespace utils {
    void helper() {}
}
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ns = chunks.iter().find(|c| c.name == "utils").unwrap();
    assert_eq!(ns.chunk_type, ChunkType::Module);
}

#[test]
fn parse_cpp_concept() {
    let content = r#"
template<typename T>
concept Printable = requires(T t) {
    { t.print() } -> std::same_as<void>;
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let concept = chunks.iter().find(|c| c.name == "Printable").unwrap();
    assert_eq!(concept.chunk_type, ChunkType::Trait);
}

#[test]
fn parse_cpp_using_alias() {
    let content = "using StringVec = std::vector<std::string>;\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "StringVec").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_cpp_typedef() {
    let content = "typedef unsigned long size_type;\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "size_type").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_cpp_method_in_class() {
    let content = r#"
class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| format!("{}:{:?}", c.name, c.chunk_type))
        .collect();
    let method = chunks
        .iter()
        .find(|c| c.name == "add")
        .unwrap_or_else(|| panic!("Expected 'add', found: {:?}", names));
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_cpp_out_of_class_method() {
    let content = r#"
class Foo {
public:
void bar();
};

void Foo::bar() {
// implementation
}
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Find the out-of-class definition (the one with a body)
    let methods: Vec<_> = chunks.iter().filter(|c| c.name == "bar").collect();
    let impl_method = methods.iter().find(|c| c.chunk_type == ChunkType::Method);
    assert!(
        impl_method.is_some(),
        "Expected out-of-class method, got: {:?}",
        methods
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        impl_method.unwrap().parent_type_name.as_deref(),
        Some("Foo")
    );
}

#[test]
fn parse_cpp_destructor_inline() {
    let content = r#"
class Resource {
public:
    ~Resource() {
        cleanup();
    }
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dtor = chunks
        .iter()
        .find(|c| c.name.contains("Resource") && c.name.contains("~"));
    assert!(
        dtor.is_some(),
        "Expected destructor, got: {:?}",
        chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    // Destructor inside class body should be Method
    assert_eq!(dtor.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn parse_cpp_destructor_out_of_class() {
    let content = r#"
class Foo {
public:
    ~Foo();
};

Foo::~Foo() {
    // cleanup
}
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dtors: Vec<_> = chunks.iter().filter(|c| c.name.contains("~")).collect();
    assert!(
        !dtors.is_empty(),
        "Expected destructor, got: {:?}",
        chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_cpp_enum_class() {
    let content = "enum class Color { Red, Green, Blue };\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Color").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_cpp_union() {
    let content = "union Data {\n  int i;\n  float f;\n};\n";
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let u = chunks.iter().find(|c| c.name == "Data").unwrap();
    assert_eq!(u.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_cpp_template_class() {
    let content = r#"
template<typename T>
class Container {
public:
    void add(T item) {}
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Container").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_cpp_calls() {
    let content = r#"
void process() {
auto x = transform(input);
obj.method();
ptr->cleanup();
auto p = std::make_shared<Foo>(42);
auto w = new Widget();
}
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform, got: {:?}",
        names
    );
    assert!(
        names.contains(&"method"),
        "Expected method, got: {:?}",
        names
    );
    assert!(
        names.contains(&"cleanup"),
        "Expected cleanup, got: {:?}",
        names
    );
}

#[test]
fn parse_cpp_constructor() {
    let content = r#"
class Widget {
public:
    Widget(int x) : x_(x) {}
    void draw() {}
private:
    int x_;
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks
        .iter()
        .find(|c| c.name == "Widget" && c.chunk_type == ChunkType::Constructor);
    assert!(
        ctor.is_some(),
        "Expected Widget constructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, c.chunk_type))
            .collect::<Vec<_>>()
    );
    // draw should still be a Method
    let method = chunks.iter().find(|c| c.name == "draw").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    // Destructor should NOT be a Constructor
}

#[test]
fn parse_cpp_destructor_not_constructor() {
    let content = r#"
class Foo {
public:
    Foo() {}
    ~Foo() {}
};
"#;
    let file = write_temp_file(content, "cpp");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dtor = chunks.iter().find(|c| c.name.starts_with('~'));
    assert!(
        dtor.is_some(),
        "Expected destructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, c.chunk_type))
            .collect::<Vec<_>>()
    );
    assert_ne!(dtor.unwrap().chunk_type, ChunkType::Constructor);
}

// -- csharp ──────────────────────────────────────────────────────────

#[test]
fn parse_csharp_constructor() {
    let content = r#"
public class Service {
    private readonly ILogger _logger;

    public Service(ILogger logger) {
        _logger = logger;
    }

    public void Run() { }
}
"#;
    let file = write_temp_file(content, "cs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks
        .iter()
        .find(|c| c.name == "Service" && c.chunk_type != ChunkType::Class)
        .unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    // Run should still be a Method
    let method = chunks.iter().find(|c| c.name == "Run").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

// -- css ─────────────────────────────────────────────────────────────

#[test]
fn parse_css_rule_set() {
    let content = r#"
.container {
    display: flex;
    padding: 16px;
}
"#;
    let file = write_temp_file(content, "css");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let rule = chunks
        .iter()
        .find(|c| c.name.contains("container") && c.chunk_type == ChunkType::Property);
    assert!(rule.is_some(), "Should find '.container' rule set");
}

#[test]
fn parse_css_keyframes() {
    let content = r#"
@keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
}
"#;
    let file = write_temp_file(content, "css");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let kf = chunks
        .iter()
        .find(|c| c.name == "spin" && c.chunk_type == ChunkType::Section);
    assert!(kf.is_some(), "Should find 'spin' keyframes as Section");
}

#[test]
fn parse_css_no_calls() {
    let content = r#"
body {
    margin: 0;
    font-family: sans-serif;
}
"#;
    let file = write_temp_file(content, "css");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "CSS should have no calls");
    }
}

// -- cuda ────────────────────────────────────────────────────────────

#[test]
fn parse_cuda_kernel() {
    let content = r#"
__global__ void vectorAdd(float *a, float *b, float *c, int n) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < n) {
        c[idx] = a[idx] + b[idx];
    }
}
"#;
    let file = write_temp_file(content, "cu");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let kernel = chunks.iter().find(|c| c.name == "vectorAdd").unwrap();
    assert_eq!(kernel.chunk_type, ChunkType::Function);
}

#[test]
fn parse_cuda_struct() {
    let content = r#"
struct DeviceConfig {
    int numBlocks;
    int threadsPerBlock;
    cudaStream_t stream;
};
"#;
    let file = write_temp_file(content, "cu");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "DeviceConfig").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_cuda_calls() {
    let content = r#"
void launch() {
    float *d_a;
    cudaMalloc(&d_a, size);
    vectorAdd<<<numBlocks, blockSize>>>(d_a, d_b, d_c, n);
    cudaDeviceSynchronize();
    cudaFree(d_a);
}
"#;
    let file = write_temp_file(content, "cu");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "launch").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"cudaMalloc"),
        "Expected cudaMalloc, got: {:?}",
        names
    );
    assert!(
        names.contains(&"cudaFree"),
        "Expected cudaFree, got: {:?}",
        names
    );
}

// -- elixir ──────────────────────────────────────────────────────────

#[test]
fn parse_elixir_function() {
    let content = r#"
defmodule MyApp do
  def greet(name) do
    "Hello, #{name}"
  end
end
"#;
    let file = write_temp_file(content, "ex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_elixir_module() {
    let content = r#"
defmodule MyApp.Users do
  def list_users do
    []
  end
end
"#;
    let file = write_temp_file(content, "ex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks
        .iter()
        .find(|c| c.name == "MyApp.Users" && c.chunk_type == ChunkType::Module);
    assert!(module.is_some(), "Should find 'MyApp.Users' module");
}

#[test]
fn parse_elixir_protocol() {
    let content = r#"
defprotocol Printable do
  def to_string(data)
end
"#;
    let file = write_temp_file(content, "ex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let proto = chunks
        .iter()
        .find(|c| c.name == "Printable" && c.chunk_type == ChunkType::Interface);
    assert!(
        proto.is_some(),
        "Should find 'Printable' protocol/interface"
    );
}

#[test]
fn parse_elixir_macro() {
    let content = r#"
defmodule MyMacros do
  defmacro my_if(condition, do: block) do
    quote do
      case unquote(condition) do
        true -> unquote(block)
        _ -> nil
      end
    end
  end
end
"#;
    let file = write_temp_file(content, "ex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let mac = chunks
        .iter()
        .find(|c| c.name == "my_if" && c.chunk_type == ChunkType::Macro);
    assert!(mac.is_some(), "Should find 'my_if' macro");
}

#[test]
fn parse_elixir_calls() {
    let content = r#"
defmodule Processor do
  def process(data) do
    data
    |> String.trim()
    |> transform()
    |> IO.puts()
  end

  defp transform(data), do: data
end
"#;
    let file = write_temp_file(content, "ex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform, got: {:?}",
        names
    );
}

// -- erlang ──────────────────────────────────────────────────────────

#[test]
fn parse_erlang_function() {
    let content = r#"
-module(mymod).
-export([greet/1]).

greet(Name) ->
    io:format("Hello, ~s~n", [Name]).
"#;
    let file = write_temp_file(content, "erl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_erlang_module() {
    let content = r#"
-module(calculator).
-export([add/2]).

add(A, B) -> A + B.
"#;
    let file = write_temp_file(content, "erl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks
        .iter()
        .find(|c| c.name == "calculator" && c.chunk_type == ChunkType::Module);
    assert!(module.is_some(), "Should find 'calculator' module");
}

#[test]
fn parse_erlang_record() {
    let content = r#"
-module(mymod).
-record(state, {count = 0, name}).

init() -> #state{count = 0, name = "test"}.
"#;
    let file = write_temp_file(content, "erl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let record = chunks
        .iter()
        .find(|c| c.name == "state" && c.chunk_type == ChunkType::Struct);
    assert!(record.is_some(), "Should find 'state' record/struct");
}

#[test]
fn parse_erlang_calls() {
    let content = r#"
-module(mymod).
-export([process/1]).

process(Data) ->
    Trimmed = string:trim(Data),
    io:format("~s~n", [Trimmed]),
    helper(Trimmed).

helper(X) -> X.
"#;
    let file = write_temp_file(content, "erl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"helper"),
        "Expected helper, got: {:?}",
        names
    );
}

#[test]
fn parse_erlang_define_macro() {
    let content = r#"
-module(mymod).
-define(MAX_RETRIES, 3).
-define(TIMEOUT, 5000).
"#;
    let file = write_temp_file(content, "erl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let retries = chunks
        .iter()
        .find(|c| c.name == "MAX_RETRIES" && c.chunk_type == ChunkType::Macro);
    assert!(
        retries.is_some(),
        "Should find MAX_RETRIES macro, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    let timeout = chunks
        .iter()
        .find(|c| c.name == "TIMEOUT" && c.chunk_type == ChunkType::Macro);
    assert!(timeout.is_some(), "Should find TIMEOUT macro");
}

// -- fsharp ──────────────────────────────────────────────────────────

#[test]
fn parse_fsharp_function() {
    let content = "let add x y = x + y\n";
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_fsharp_record() {
    let content = r#"
type Person = {
    Name: string
    Age: int
}
"#;
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let record = chunks.iter().find(|c| c.name == "Person").unwrap();
    assert_eq!(record.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_fsharp_discriminated_union() {
    let content = r#"
type Shape =
    | Circle of float
    | Rectangle of float * float
"#;
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let du = chunks.iter().find(|c| c.name == "Shape").unwrap();
    assert_eq!(du.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_fsharp_class_and_method() {
    let content = r#"
type Calculator() =
    member this.Add(a: int, b: int) : int =
        a + b
"#;
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
    let method = chunks.iter().find(|c| c.name == "Add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_fsharp_interface() {
    // F# interfaces with [<Interface>] attribute use interface_type_defn.
    // Without the attribute, tree-sitter-fsharp parses them as anon_type_defn (Class).
    // This is correct F# behavior — abstract classes and interfaces are both valid
    // without [<Interface>]. We accept Class for unattributed abstract types.
    let content = r#"
type ILogger =
abstract member Log: string -> unit
"#;
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let iface = chunks.iter().find(|c| c.name == "ILogger").unwrap();
    // Without [<Interface>] attribute, tree-sitter classifies as anon_type_defn → Class
    assert_eq!(iface.chunk_type, ChunkType::Class);
}

#[test]
fn parse_fsharp_module() {
    let content = r#"
module Helpers =
let helper x = x + 1
"#;
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks.iter().find(|c| c.name == "Helpers").unwrap();
    assert_eq!(module.chunk_type, ChunkType::Module);
}

#[test]
fn parse_fsharp_type_abbreviation() {
    // F# type abbreviation: `type X = ExistingType`
    // Note: `type Name = string` is parsed as union_type_defn by tree-sitter-fsharp
    // because bare lowercase identifiers are ambiguous. Use function types to test.
    let content = "type Callback = int -> string\n";
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "Callback").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_fsharp_calls() {
    let content = r#"
let processData (input: string) : int =
    let trimmed = input.Trim()
    let parsed = Int32.Parse(trimmed)
    add parsed 1
"#;
    let parser = Parser::new().unwrap();
    let lang = Language::FSharp;
    let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    // Dot calls
    assert!(
        names.contains(&"Trim"),
        "Expected Trim call, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Parse"),
        "Expected Parse call, got: {:?}",
        names
    );
}

#[test]
fn parse_fsharp_type_extension() {
    let content = "type MyRecord with\n    member x.Greet() = \"hello\"\n";
    let file = write_temp_file(content, "fs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ext = chunks.iter().find(|c| c.chunk_type == ChunkType::Extension);
    assert!(
        ext.is_some(),
        "Expected a type extension chunk, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    let ext = ext.unwrap();
    assert_eq!(ext.name, "MyRecord");
    assert_eq!(ext.chunk_type, ChunkType::Extension);
}

// -- gleam ───────────────────────────────────────────────────────────

#[test]
fn parse_gleam_function() {
    let content = r#"
pub fn add(x: Int, y: Int) -> Int {
  x + y
}
"#;
    let file = write_temp_file(content, "gleam");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_gleam_type() {
    let content = r#"
pub type Color {
  Red
  Green
  Blue
}
"#;
    let file = write_temp_file(content, "gleam");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dt = chunks
        .iter()
        .find(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum);
    assert!(dt.is_some(), "Should find 'Color' type as Enum");
}

#[test]
fn parse_gleam_type_alias() {
    let content = r#"
pub type UserId = Int
"#;
    let file = write_temp_file(content, "gleam");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks
        .iter()
        .find(|c| c.name == "UserId" && c.chunk_type == ChunkType::TypeAlias);
    assert!(ta.is_some(), "Should find 'UserId' type alias");
}

#[test]
fn parse_gleam_constant() {
    let content = r#"
pub const max_retries: Int = 3
"#;
    let file = write_temp_file(content, "gleam");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let c = chunks
        .iter()
        .find(|c| c.name == "max_retries" && c.chunk_type == ChunkType::Constant);
    assert!(c.is_some(), "Should find 'max_retries' constant");
}

#[test]
fn parse_gleam_calls() {
    let content = r#"
import gleam/io

pub fn main() {
  let result = add(1, 2)
  io.println("done")
}

fn add(x: Int, y: Int) -> Int {
  x + y
}
"#;
    let file = write_temp_file(content, "gleam");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "main").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"add"), "Expected add, got: {:?}", names);
}

// -- glsl ────────────────────────────────────────────────────────────

#[test]
fn parse_glsl_vertex_shader() {
    let content = r#"
#version 450

layout(location = 0) in vec3 aPosition;
layout(location = 1) in vec2 aTexCoord;

layout(location = 0) out vec2 vTexCoord;

uniform mat4 uModelViewProjection;

void main() {
    gl_Position = uModelViewProjection * vec4(aPosition, 1.0);
    vTexCoord = aTexCoord;
}
"#;
    let file = write_temp_file(content, "vert");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let main_fn = chunks.iter().find(|c| c.name == "main").unwrap();
    assert_eq!(main_fn.chunk_type, ChunkType::Function);
}

#[test]
fn parse_glsl_struct() {
    let content = r#"
struct Light {
    vec3 position;
    vec3 color;
    float intensity;
};
"#;
    let file = write_temp_file(content, "glsl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Light").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_glsl_calls() {
    let content = r#"
vec4 applyLighting(vec3 normal, vec3 lightDir) {
    float diff = max(dot(normal, lightDir), 0.0);
    vec3 color = mix(ambient, diffuse, diff);
    return vec4(normalize(color), 1.0);
}
"#;
    let file = write_temp_file(content, "frag");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "applyLighting").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"max"), "Expected max, got: {:?}", names);
    assert!(names.contains(&"dot"), "Expected dot, got: {:?}", names);
    assert!(names.contains(&"mix"), "Expected mix, got: {:?}", names);
    assert!(
        names.contains(&"normalize"),
        "Expected normalize, got: {:?}",
        names
    );
}

// -- go ──────────────────────────────────────────────────────────────

#[test]
fn parse_go_named_type() {
    let content = "package main\n\ntype MyInt int\n";
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_go_function_type() {
    let content = "package main\n\ntype Handler func(w Writer, r *Request)\n";
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "Handler").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_go_type_alias_equals() {
    let content = "package main\n\ntype MyInt = int\n";
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_go_struct_still_struct() {
    // Ensure struct type declarations are NOT captured as TypeAlias
    let content = "package main\n\ntype Foo struct {\n\tX int\n}\n";
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Foo").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_go_constructor() {
    let content = r#"
package main

type Server struct {
    Port int
}

func NewServer(port int) *Server {
    return &Server{Port: port}
}

func helper() {}
"#;
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks.iter().find(|c| c.name == "NewServer").unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    // helper should still be a Function
    let func = chunks.iter().find(|c| c.name == "helper").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

// -- graphql ─────────────────────────────────────────────────────────

#[test]
fn parse_graphql_object_type() {
    let content = r#"
type User {
  id: ID!
  name: String!
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let user = chunks.iter().find(|c| c.name == "User").unwrap();
    assert_eq!(user.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_graphql_interface() {
    let content = r#"
interface Node {
  id: ID!
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let node = chunks.iter().find(|c| c.name == "Node").unwrap();
    assert_eq!(node.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_graphql_enum() {
    let content = r#"
enum Status {
  ACTIVE
  INACTIVE
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Status").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_graphql_union() {
    let content = "union SearchResult = User | Post\n";
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let u = chunks.iter().find(|c| c.name == "SearchResult").unwrap();
    assert_eq!(u.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_graphql_input() {
    let content = r#"
input CreateUserInput {
  name: String!
  email: String!
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let input = chunks.iter().find(|c| c.name == "CreateUserInput").unwrap();
    assert_eq!(input.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_graphql_scalar() {
    let content = "scalar DateTime\n";
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "DateTime").unwrap();
    assert_eq!(s.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_graphql_directive() {
    let content = "directive @auth(requires: Role!) on FIELD_DEFINITION\n";
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let d = chunks.iter().find(|c| c.name == "auth").unwrap();
    assert_eq!(d.chunk_type, ChunkType::Macro);
}

#[test]
fn parse_graphql_operation() {
    let content = r#"
query GetUser($id: ID!) {
  user(id: $id) {
    name
  }
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let op = chunks.iter().find(|c| c.name == "GetUser").unwrap();
    assert_eq!(op.chunk_type, ChunkType::Function);
}

#[test]
fn parse_graphql_fragment() {
    let content = r#"
fragment UserFields on User {
  name
  email
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let frag = chunks.iter().find(|c| c.name == "UserFields").unwrap();
    assert_eq!(frag.chunk_type, ChunkType::Function);
}

#[test]
fn parse_graphql_calls() {
    let content = r#"
type User {
  id: ID!
  posts: [Post!]!
  address: Address
}
"#;
    let file = write_temp_file(content, "graphql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let user = chunks.iter().find(|c| c.name == "User").unwrap();
    let calls = parser.extract_calls_from_chunk(user);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"Post"),
        "Expected Post type reference, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Address"),
        "Expected Address type reference, got: {:?}",
        names
    );
}

// -- haskell ─────────────────────────────────────────────────────────

#[test]
fn parse_haskell_function() {
    let content = r#"
greet :: String -> String
greet name = "Hello, " ++ name
"#;
    let file = write_temp_file(content, "hs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_haskell_data_type() {
    let content = r#"
data Color = Red | Green | Blue
"#;
    let file = write_temp_file(content, "hs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dt = chunks
        .iter()
        .find(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum);
    assert!(dt.is_some(), "Should find 'Color' data type as Enum");
}

#[test]
fn parse_haskell_typeclass() {
    let content = r#"
class Printable a where
  display :: a -> String
"#;
    let file = write_temp_file(content, "hs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let tc = chunks
        .iter()
        .find(|c| c.name == "Printable" && c.chunk_type == ChunkType::Trait);
    assert!(tc.is_some(), "Should find 'Printable' typeclass as Trait");
}

#[test]
fn parse_haskell_instance() {
    let content = r#"
data Color = Red | Green | Blue

instance Show Color where
  show Red = "Red"
  show Green = "Green"
  show Blue = "Blue"
"#;
    let file = write_temp_file(content, "hs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let inst = chunks
        .iter()
        .find(|c| c.name == "Show" && c.chunk_type == ChunkType::Impl);
    assert!(
        inst.is_some(),
        "Should find 'Show' instance as Impl, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_haskell_calls() {
    let content = r#"
process :: String -> IO ()
process text = do
  let trimmed = trim text
  putStrLn trimmed
  validate trimmed
"#;
    let file = write_temp_file(content, "hs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"putStrLn"),
        "Expected putStrLn, got: {:?}",
        names
    );
}

// -- hcl ─────────────────────────────────────────────────────────────

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

// --- Injection tests ---

#[test]
fn parse_hcl_heredoc_bash() {
    // Heredoc with EOT identifier should be parsed as bash
    let content = r#"
resource "null_resource" "setup" {
  provisioner "local-exec" {
    command = <<-EOT
      #!/bin/bash
      echo "Setting up environment"
      mkdir -p /tmp/deploy
    EOT
  }
}
"#;
    let file = write_temp_file(content, "tf");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // HCL resource should still exist
    assert!(
        chunks.iter().any(|c| c.language == Language::Hcl),
        "Expected HCL chunks to survive injection"
    );
}

#[test]
fn parse_hcl_heredoc_non_bash_skipped() {
    // Heredoc with unrecognized identifier should be skipped
    let content = r#"
resource "aws_instance" "web" {
  user_data = <<-CLOUDINIT
    #cloud-config
    packages:
      - nginx
  CLOUDINIT
}
"#;
    let file = write_temp_file(content, "tf");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // No bash chunks — CLOUDINIT is not a shell identifier
    let bash_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Bash)
        .collect();
    assert!(
        bash_chunks.is_empty(),
        "Unrecognized heredoc identifier should NOT produce bash chunks"
    );
}

#[test]
fn parse_hcl_without_heredocs_unchanged() {
    // HCL file with no heredocs — injection should not fire
    let content = r#"
variable "name" {
  type = string
}

output "greeting" {
  value = "Hello, ${var.name}"
}
"#;
    let file = write_temp_file(content, "tf");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Hcl,
            "File without heredocs should only have HCL chunks"
        );
    }
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

// -- html ────────────────────────────────────────────────────────────

#[test]
fn parse_html_heading_as_section() {
    let content = r#"<h1>Welcome to My Site</h1>
<p>Some paragraph text</p>
<h2>About</h2>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "Welcome to My Site" && c.chunk_type == ChunkType::Section),
        "Expected h1 as Section, got: {:?}",
        names
    );
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "About" && c.chunk_type == ChunkType::Section),
        "Expected h2 as Section, got: {:?}",
        names
    );
}

#[test]
fn parse_html_script_as_module() {
    let content = r#"<html>
<head><title>Test</title></head>
<body>
<script src="app.js"></script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let script = chunks.iter().find(|c| c.chunk_type == ChunkType::Module);
    assert!(
        script.is_some(),
        "Expected script as Module, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    assert!(script.unwrap().name.contains("app.js"));
}

#[test]
fn parse_html_landmark_as_section() {
    let content = r#"<nav id="main-nav">
  <a href="/">Home</a>
</nav>
<main>
  <article>Content here</article>
</main>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let nav = chunks
        .iter()
        .find(|c| c.name.contains("main-nav") && c.chunk_type == ChunkType::Section);
    assert!(
        nav.is_some(),
        "Expected nav#main-nav as Section, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_noise_filtered() {
    let content = r#"<div>
  <span>text</span>
  <p>paragraph</p>
</div>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // div, span, p are all noise — should be filtered out
    assert!(
        chunks.is_empty(),
        "Expected noise elements filtered, got: {:?}",
        chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_div_with_id_kept() {
    let content = r#"<div id="app">
  <p>content</p>
</div>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let app = chunks
        .iter()
        .find(|c| c.name == "div#app" && c.chunk_type == ChunkType::Property);
    assert!(
        app.is_some(),
        "Expected div#app as ConfigKey, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_no_calls() {
    let content = "<h1>Title</h1>\n";
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "HTML should have no calls");
    }
}

#[test]
fn parse_html_with_script_extracts_js_functions() {
    let content = r#"<html>
<body>
<h1>Title</h1>
<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function setupListeners() {
    handleClick(null);
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should have JS function chunks
    let js_funcs: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_funcs.iter().any(|c| c.name == "handleClick"),
        "Expected JS function 'handleClick', got: {:?}",
        js_funcs.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        js_funcs.iter().any(|c| c.name == "setupListeners"),
        "Expected JS function 'setupListeners', got: {:?}",
        js_funcs.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    // JS functions should have correct language
    for f in &js_funcs {
        assert_eq!(f.language, Language::JavaScript);
        assert_eq!(f.chunk_type, ChunkType::Function);
    }

    // HTML heading should still be present
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "Title" && c.chunk_type == ChunkType::Section),
        "Expected HTML heading 'Title'"
    );

    // The script Module chunk should have been replaced by JS functions
    assert!(
        !chunks
            .iter()
            .any(|c| c.chunk_type == ChunkType::Module && c.name == "script"),
        "Script Module chunk should be replaced by JS functions"
    );
}

#[test]
fn parse_html_with_style_extracts_css_rules() {
    let content = r#"<html>
<head>
<style>
.container {
    display: flex;
    gap: 1rem;
}
</style>
</head>
<body><h1>Page</h1></body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // CSS chunks should be extracted (if CSS query captures rules)
    let css_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Css)
        .collect();

    // CSS injection must produce chunks — if this fails, CSS injection is broken
    assert!(
        !css_chunks.is_empty(),
        "CSS injection should extract chunks from <style> block"
    );
    // Style Module chunk should be replaced by CSS chunks
    assert!(
        !chunks
            .iter()
            .any(|c| c.chunk_type == ChunkType::Module && c.name == "style"),
        "Style Module chunk should be replaced by CSS chunks"
    );

    // HTML heading should still be present
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "Page" && c.chunk_type == ChunkType::Section),
        "Expected HTML heading 'Page'"
    );
}

#[test]
fn parse_html_with_typescript_script() {
    let content = r#"<html>
<body>
<script lang="ts">
function typedFunction(x: number): string {
    return x.toString();
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let ts_funcs: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::TypeScript)
        .collect();
    assert!(
        ts_funcs.iter().any(|c| c.name == "typedFunction"),
        "Expected TypeScript function, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_with_empty_script_keeps_module() {
    // <script src="..."> has no raw_text child — should keep outer Module
    let content = r#"<html>
<body>
<script src="app.js"></script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let module = chunks.iter().find(|c| c.chunk_type == ChunkType::Module);
    assert!(
        module.is_some(),
        "Empty script should keep Module chunk, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_with_multiple_scripts() {
    let content = r#"<html>
<body>
<script>
function first() { return 1; }
</script>
<script>
function second() { return 2; }
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let js_names: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        js_names.contains(&"first"),
        "Expected 'first' from first script, got: {:?}",
        js_names
    );
    assert!(
        js_names.contains(&"second"),
        "Expected 'second' from second script, got: {:?}",
        js_names
    );
}

#[test]
fn parse_html_with_whitespace_only_script_keeps_module() {
    let content = "<html><body>\n<script>  \n  </script>\n</body></html>\n";
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Whitespace-only script produces zero inner chunks — should keep outer
    let has_module = chunks.iter().any(|c| c.chunk_type == ChunkType::Module);
    assert!(
        has_module,
        "Whitespace-only script should keep Module chunk, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_html_without_script_unchanged() {
    // HTML with only headings/nav — no injections should fire
    let content = r#"<html>
<body>
<nav id="main-nav"><a href="/">Home</a></nav>
<h1>Welcome</h1>
<h2>About</h2>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should have only HTML chunks
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Html,
            "All chunks should be HTML, found {:?} for '{}'",
            chunk.language,
            chunk.name
        );
    }

    // Verify expected chunks
    assert!(chunks.iter().any(|c| c.name == "Welcome"));
    assert!(chunks.iter().any(|c| c.name == "About"));
    assert!(chunks.iter().any(|c| c.name.contains("main-nav")));
}

#[test]
fn injection_call_graph() {
    let content = r#"<html>
<body>
<script>
function caller() {
    helper();
    other();
}

function helper() {
    return 42;
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

    let caller_calls = calls.iter().find(|c| c.name == "caller");
    assert!(
        caller_calls.is_some(),
        "Expected call graph entry for 'caller', got: {:?}",
        calls.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let call_names: Vec<_> = caller_calls
        .unwrap()
        .calls
        .iter()
        .map(|c| c.callee_name.as_str())
        .collect();
    assert!(
        call_names.contains(&"helper"),
        "Expected caller → helper, got: {:?}",
        call_names
    );
    assert!(
        call_names.contains(&"other"),
        "Expected caller → other, got: {:?}",
        call_names
    );
}

#[test]
fn parse_html_with_type_text_typescript() {
    // type="text/typescript" should also trigger TypeScript parsing
    let content = r#"<html>
<body>
<script type="text/typescript">
function typedFunc(x: number): string {
    return String(x);
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let ts_funcs: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::TypeScript)
        .collect();
    assert!(
        ts_funcs.iter().any(|c| c.name == "typedFunc"),
        "Expected TypeScript function from type=\"text/typescript\", got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn injection_type_refs_extracted() {
    // TypeScript inside HTML should produce type references
    let content = r#"<html>
<body>
<script lang="ts">
function process(config: Config): StoreError {
    return {} as StoreError;
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

    // Should have type refs from the injected TypeScript
    let process_types = types.iter().find(|t| t.name == "process");
    assert!(
        process_types.is_some(),
        "Expected type refs for 'process', got names: {:?}",
        types.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    let refs = &process_types.unwrap().type_refs;
    assert!(
        refs.iter().any(|t| t.type_name == "Config"),
        "Expected Config type ref, got: {:?}",
        refs
    );
    assert!(
        refs.iter().any(|t| t.type_name == "StoreError"),
        "Expected StoreError type ref, got: {:?}",
        refs
    );
}

#[test]
fn parse_html_with_unclosed_script() {
    // Malformed HTML: unclosed <script> tag — error recovery should still work
    let content = r#"<html>
<body>
<h1>Title</h1>
<script>
function broken() { return 1; }
</body>
</html>
"#;
    let file = write_temp_file(content, "html");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should not panic — parser should produce some result
    // HTML heading should still be present
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "Title" && c.chunk_type == ChunkType::Section),
        "HTML heading should survive malformed script, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn injection_ranges_empty_for_non_injection_language() {
    // Rust files have no injection rules — should return empty
    let content = "fn main() {}\n";
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].language, Language::Rust);
}

// -- ini ─────────────────────────────────────────────────────────────

#[test]
fn parse_ini_sections() {
    let content = r#"[database]
host = localhost
port = 5432

[server]
host = 0.0.0.0
port = 8080
"#;
    let file = write_temp_file(content, "ini");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "database" && c.chunk_type == ChunkType::Module),
        "Expected 'database' section as Module, got: {:?}",
        names
    );
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "server" && c.chunk_type == ChunkType::Module),
        "Expected 'server' section as Module, got: {:?}",
        names
    );
}

#[test]
fn parse_ini_settings() {
    let content = r#"[app]
debug = true
log_level = info
"#;
    let file = write_temp_file(content, "ini");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let debug = chunks
        .iter()
        .find(|c| c.name == "debug" && c.chunk_type == ChunkType::ConfigKey);
    assert!(
        debug.is_some(),
        "Expected 'debug' setting as ConfigKey, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_ini_no_calls() {
    let content = "[section]\nkey = value\n";
    let file = write_temp_file(content, "ini");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "INI should have no calls");
    }
}

// -- java ────────────────────────────────────────────────────────────

#[test]
fn parse_java_annotation_type() {
    let content = r#"
public @interface Inject {
    String value() default "";
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ann = chunks.iter().find(|c| c.name == "Inject").unwrap();
    assert_eq!(ann.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_java_field_as_property() {
    let content = r#"
public class Config {
    private String name;
    public static final int MAX_SIZE = 100;
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let field = chunks.iter().find(|c| c.name == "name").unwrap();
    assert_eq!(field.chunk_type, ChunkType::Property);
    // static final fields should be Constant, not Property
    let constant = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
    assert_eq!(constant.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_java_constructor() {
    let content = r#"
public class Person {
    private String name;

    public Person(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks
        .iter()
        .find(|c| c.name == "Person" && c.chunk_type != ChunkType::Class)
        .unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    // getName should still be a Method
    let method = chunks.iter().find(|c| c.name == "getName").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

// -- javascript ──────────────────────────────────────────────────────

#[test]
fn parse_javascript_const_value() {
    let content = r#"
const MAX_RETRIES = 3;
const API_URL = "https://example.com";
const handler = () => { return 1; };

function foo() {}
"#;
    let file = write_temp_file(content, "js");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(max.is_some(), "Should capture MAX_RETRIES");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
    let url = chunks.iter().find(|c| c.name == "API_URL");
    assert!(url.is_some(), "Should capture API_URL");
    assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
    // handler is an arrow function — should be Function, not Constant
    let handler = chunks.iter().find(|c| c.name == "handler");
    assert!(handler.is_some(), "Should capture handler");
    assert_eq!(handler.unwrap().chunk_type, ChunkType::Function);
}

// -- json ────────────────────────────────────────────────────────────

#[test]
fn parse_json_top_level_keys() {
    let content = r#"{
  "name": "my-project",
  "version": "1.0.0",
  "dependencies": {
    "lodash": "4.17.21"
  }
}
"#;
    let file = write_temp_file(content, "json");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"name"),
        "Expected 'name' key, got: {:?}",
        names
    );
    assert!(
        names.contains(&"version"),
        "Expected 'version' key, got: {:?}",
        names
    );
    assert!(
        names.contains(&"dependencies"),
        "Expected 'dependencies' key, got: {:?}",
        names
    );
    // Nested key "lodash" should be filtered out
    assert!(
        !names.contains(&"lodash"),
        "Nested key 'lodash' should be filtered, got: {:?}",
        names
    );
}

#[test]
fn parse_json_chunk_type() {
    let content = r#"{"key": "value"}"#;
    let file = write_temp_file(content, "json");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let key = chunks.iter().find(|c| c.name == "key");
    assert!(key.is_some(), "Expected 'key' chunk");
    assert_eq!(key.unwrap().chunk_type, ChunkType::ConfigKey);
}

#[test]
fn parse_json_no_calls() {
    let content = r#"{"a": 1, "b": 2}"#;
    let file = write_temp_file(content, "json");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "JSON should have no calls");
    }
}

// -- julia ───────────────────────────────────────────────────────────

#[test]
fn parse_julia_function() {
    let content = r#"
function add(x, y)
    return x + y
end
"#;
    let file = write_temp_file(content, "jl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_julia_struct() {
    let content = r#"
struct Point
    x::Float64
    y::Float64
end
"#;
    let file = write_temp_file(content, "jl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks
        .iter()
        .find(|c| c.name == "Point" && c.chunk_type == ChunkType::Struct);
    assert!(s.is_some(), "Should find 'Point' struct");
}

#[test]
fn parse_julia_module() {
    let content = r#"
module Calculator
    function add(x, y)
        return x + y
    end
end
"#;
    let file = write_temp_file(content, "jl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks
        .iter()
        .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
    assert!(module.is_some(), "Should find 'Calculator' module");
}

#[test]
fn parse_julia_abstract_type() {
    let content = r#"
abstract type Shape end
"#;
    let file = write_temp_file(content, "jl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let at = chunks
        .iter()
        .find(|c| c.name == "Shape" && c.chunk_type == ChunkType::TypeAlias);
    assert!(at.is_some(), "Should find 'Shape' abstract type");
}

#[test]
fn parse_julia_calls() {
    let content = r#"
function process(data)
    result = transform(data)
    println(result)
    validate(result)
end
"#;
    let file = write_temp_file(content, "jl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform, got: {:?}",
        names
    );
}

// -- kotlin ──────────────────────────────────────────────────────────

#[test]
fn parse_kotlin_data_class() {
    let content = r#"
data class Person(val name: String, val age: Int)
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Person").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_kotlin_interface() {
    let content = r#"
interface Printable {
    fun print()
    fun prettyPrint()
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
    assert_eq!(iface.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_kotlin_enum_class() {
    let content = r#"
enum class Color {
    RED, GREEN, BLUE
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Color").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_kotlin_object() {
    let content = r#"
object Singleton {
fun greet(): String = "hello"
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let obj = chunks.iter().find(|c| c.name == "Singleton").unwrap();
    assert_eq!(obj.chunk_type, ChunkType::Object);
}

#[test]
fn parse_kotlin_function() {
    let content = r#"
fun add(a: Int, b: Int): Int {
    return a + b
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_kotlin_typealias() {
    let content = "typealias StringMap = Map<String, String>\n";
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_kotlin_calls() {
    let content = r#"
fun process(input: String): Int {
    val trimmed = input.trim()
    val result = parseInt(trimmed)
    println(result)
    return result
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"parseInt"),
        "Expected parseInt call, got: {:?}",
        names
    );
    assert!(
        names.contains(&"println"),
        "Expected println call, got: {:?}",
        names
    );
}

#[test]
fn parse_kotlin_property() {
    let content = r#"
val greeting: String = "hello"
var counter: Int = 0
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let val_chunk = chunks.iter().find(|c| c.name == "greeting").unwrap();
    assert_eq!(val_chunk.chunk_type, ChunkType::Property);
    let var_chunk = chunks.iter().find(|c| c.name == "counter").unwrap();
    assert_eq!(var_chunk.chunk_type, ChunkType::Property);
}

#[test]
fn parse_kotlin_method_in_class() {
    let content = r#"
class Calculator {
fun add(a: Int, b: Int): Int {
    return a + b
}
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_kotlin_sealed_class() {
    let content = r#"
sealed class Result {
data class Success(val data: String) : Result()
data class Error(val message: String) : Result()
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let sealed = chunks.iter().find(|c| c.name == "Result").unwrap();
    assert_eq!(sealed.chunk_type, ChunkType::Class);
}

#[test]
fn parse_kotlin_secondary_constructor() {
    let content = r#"
class MyClass(val name: String) {
    constructor(x: Int) : this(x.toString())
    fun greet() { println("hi") }
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks
        .iter()
        .find(|c| c.name == "constructor" && c.chunk_type == ChunkType::Constructor);
    assert!(
        ctor.is_some(),
        "Expected secondary constructor as Constructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, c.chunk_type))
            .collect::<Vec<_>>()
    );
    // greet should still be a Method
    let method = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

#[test]
fn parse_kotlin_init_block() {
    let content = r#"
class Config(val path: String) {
    init {
        println("loading config")
    }
    fun load() { }
}
"#;
    let file = write_temp_file(content, "kt");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let init = chunks
        .iter()
        .find(|c| c.name == "init" && c.chunk_type == ChunkType::Constructor);
    assert!(
        init.is_some(),
        "Expected init block as Constructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, c.chunk_type))
            .collect::<Vec<_>>()
    );
}

// -- latex ───────────────────────────────────────────────────────────

#[test]
fn parse_latex_sections() {
    let content = r#"\documentclass{article}
\begin{document}

\section{Introduction}
This is the introduction.

\subsection{Background}
Some background information.

\section{Methods}
The methods section.

\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"Introduction"),
        "Expected 'Introduction' section, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Background"),
        "Expected 'Background' subsection, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Methods"),
        "Expected 'Methods' section, got: {:?}",
        names
    );
    let intro = chunks.iter().find(|c| c.name == "Introduction").unwrap();
    assert_eq!(intro.chunk_type, ChunkType::Section);
}

#[test]
fn parse_latex_command_definition() {
    let content = r#"\documentclass{article}

\newcommand{\highlight}[1]{\textbf{#1}}
\newcommand{\todo}[1]{\textcolor{red}{TODO: #1}}

\begin{document}
\highlight{Important text}
\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"highlight"),
        "Expected 'highlight' command, got: {:?}",
        names
    );
    assert!(
        names.contains(&"todo"),
        "Expected 'todo' command, got: {:?}",
        names
    );
    let cmd = chunks.iter().find(|c| c.name == "highlight").unwrap();
    assert_eq!(cmd.chunk_type, ChunkType::Function);
}

#[test]
fn parse_latex_environment() {
    let content = r#"\documentclass{article}
\begin{document}

\begin{theorem}
Every even integer greater than 2 can be expressed as the sum of two primes.
\end{theorem}

\begin{proof}
This is left as an exercise.
\end{proof}

\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"theorem"),
        "Expected 'theorem' environment, got: {:?}",
        names
    );
    assert!(
        names.contains(&"proof"),
        "Expected 'proof' environment, got: {:?}",
        names
    );
    let thm = chunks.iter().find(|c| c.name == "theorem").unwrap();
    assert_eq!(thm.chunk_type, ChunkType::Struct);
}

// --- Injection tests ---

#[test]
fn parse_latex_minted_extracts_code() {
    // \begin{minted}{python} should inject Python
    let content = r#"\documentclass{article}
\usepackage{minted}
\begin{document}

\section{Code Example}

\begin{minted}{python}
def greet(name):
    return f"Hello, {name}!"

class Calculator:
    def add(self, a, b):
        return a + b
\end{minted}

\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Python chunks should be extracted
    let py_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Python)
        .collect();
    assert!(
        py_chunks.iter().any(|c| c.name == "greet"),
        "Expected Python function 'greet' from minted block, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );

    // LaTeX section should still exist
    assert!(
        chunks.iter().any(|c| c.name == "Code Example"),
        "Expected LaTeX section 'Code Example'"
    );
}

#[test]
fn parse_latex_listing_extracts_code() {
    // \begin{lstlisting}[language=Rust] should inject Rust
    let content = r#"\documentclass{article}
\usepackage{listings}
\begin{document}

\section{Rust Example}

\begin{lstlisting}[language=Rust]
fn main() {
    println!("Hello, world!");
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}
\end{lstlisting}

\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Rust chunks should be extracted
    let rust_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Rust)
        .collect();
    assert!(
        !rust_chunks.is_empty(),
        "Expected Rust chunks from lstlisting[language=Rust], got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_latex_without_listings_unchanged() {
    // LaTeX file with no code listings — injection should not fire
    let content = r#"\documentclass{article}
\begin{document}
\section{Introduction}
Hello world.
\section{Methods}
Some methods.
\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Latex,
            "File without code listings should only have LaTeX chunks"
        );
    }
}

#[test]
fn parse_latex_no_calls() {
    let content = r#"\documentclass{article}
\begin{document}
\section{Test}
Hello world.
\end{document}
"#;
    let file = write_temp_file(content, "tex");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "LaTeX should have no calls");
    }
}

// -- lua ─────────────────────────────────────────────────────────────

#[test]
fn parse_lua_function() {
    let content = r#"
function greet(name)
    print("Hello, " .. name)
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_lua_local_function() {
    let content = r#"
local function helper(x)
    return x * 2
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "helper").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_lua_calls() {
    let content = r#"
function process(data)
local trimmed = string.trim(data)
print(trimmed)
return tonumber(trimmed)
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"print"), "Expected print, got: {:?}", names);
    assert!(
        names.contains(&"tonumber"),
        "Expected tonumber, got: {:?}",
        names
    );
}

#[test]
fn parse_lua_method_call() {
    let content = r#"
function setup(obj)
    obj:init()
    obj:configure("default")
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "setup").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"init"), "Expected init, got: {:?}", names);
    assert!(
        names.contains(&"configure"),
        "Expected configure, got: {:?}",
        names
    );
}

#[test]
fn parse_lua_local_constant() {
    let content = r#"
local MAX_SIZE = 100
local API_URL = "https://example.com"
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
    assert_eq!(max.chunk_type, ChunkType::Constant);
    let url = chunks.iter().find(|c| c.name == "API_URL").unwrap();
    assert_eq!(url.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_lua_global_constant() {
    let content = r#"
MAX_RETRIES = 3
DEFAULT_TIMEOUT = 30
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let retries = chunks.iter().find(|c| c.name == "MAX_RETRIES").unwrap();
    assert_eq!(retries.chunk_type, ChunkType::Constant);
    let timeout = chunks.iter().find(|c| c.name == "DEFAULT_TIMEOUT").unwrap();
    assert_eq!(timeout.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_lua_skip_lowercase_vars() {
    let content = r#"
local counter = 0
local myTable = {}
helper_value = 42
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // lowercase names should be filtered out by post_process
    assert!(chunks.iter().find(|c| c.name == "counter").is_none());
    assert!(chunks.iter().find(|c| c.name == "myTable").is_none());
    assert!(chunks.iter().find(|c| c.name == "helper_value").is_none());
}

#[test]
fn parse_lua_skip_constants_inside_functions() {
    let content = r#"
function init()
    local MAX_LOCAL = 50
    GLOBAL_IN_FUNC = 99
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Constants inside function bodies should be skipped
    assert!(chunks.iter().find(|c| c.name == "MAX_LOCAL").is_none());
    assert!(chunks.iter().find(|c| c.name == "GLOBAL_IN_FUNC").is_none());
    // But the function itself should be captured
    let func = chunks.iter().find(|c| c.name == "init").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_lua_skip_function_assigned_to_var() {
    let content = r#"
local MY_HANDLER = function(x)
    return x * 2
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Function-valued assignments should not become constants
    assert!(chunks
        .iter()
        .find(|c| c.name == "MY_HANDLER" && c.chunk_type == ChunkType::Constant)
        .is_none());
}

#[test]
fn parse_lua_mixed_functions_and_constants() {
    let content = r#"
local VERSION = "1.0.0"
MAX_BUFFER = 4096

function process(data)
    return data
end

local function helper()
    return true
end
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();
    assert!(
        names.contains(&("VERSION", ChunkType::Constant)),
        "Expected VERSION constant, got: {:?}",
        names
    );
    assert!(
        names.contains(&("MAX_BUFFER", ChunkType::Constant)),
        "Expected MAX_BUFFER constant, got: {:?}",
        names
    );
    assert!(
        names.contains(&("process", ChunkType::Function)),
        "Expected process function, got: {:?}",
        names
    );
    assert!(
        names.contains(&("helper", ChunkType::Function)),
        "Expected helper function, got: {:?}",
        names
    );
}

#[test]
fn parse_lua_no_duplicate_constants() {
    let content = r#"
local MAX_SIZE = 100
MAX_RETRIES = 3
"#;
    let file = write_temp_file(content, "lua");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Each constant should appear exactly once
    let max_count = chunks.iter().filter(|c| c.name == "MAX_SIZE").count();
    let retries_count = chunks.iter().filter(|c| c.name == "MAX_RETRIES").count();
    assert_eq!(
        max_count, 1,
        "MAX_SIZE should appear once, got {}",
        max_count
    );
    assert_eq!(
        retries_count, 1,
        "MAX_RETRIES should appear once, got {}",
        retries_count
    );
}

// -- make ────────────────────────────────────────────────────────────

#[test]
fn parse_make_rule() {
    let content = r#"
all: build test
	echo "Done"

build: src/main.c
	gcc -o main src/main.c

test: build
	./run_tests
"#;
    let file = write_temp_file(content, "mk");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"all"),
        "Expected 'all' rule, got: {:?}",
        names
    );
    assert!(
        names.contains(&"build"),
        "Expected 'build' rule, got: {:?}",
        names
    );
    assert!(
        names.contains(&"test"),
        "Expected 'test' rule, got: {:?}",
        names
    );
    let build = chunks.iter().find(|c| c.name == "build").unwrap();
    assert_eq!(build.chunk_type, ChunkType::Function);
}

#[test]
fn parse_make_variable() {
    let content = r#"
CC = gcc
CFLAGS = -Wall -Werror
SRC = $(wildcard src/*.c)

all: $(SRC)
	$(CC) $(CFLAGS) -o main $(SRC)
"#;
    let file = write_temp_file(content, "mk");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"CC"),
        "Expected 'CC' variable, got: {:?}",
        names
    );
    assert!(
        names.contains(&"CFLAGS"),
        "Expected 'CFLAGS' variable, got: {:?}",
        names
    );
    let cc = chunks.iter().find(|c| c.name == "CC").unwrap();
    assert_eq!(cc.chunk_type, ChunkType::Property);
}

#[test]
fn parse_make_no_calls() {
    let content = r#"
clean:
	rm -rf build/
"#;
    let file = write_temp_file(content, "mk");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "Make should have no call graph");
    }
}

#[test]
fn parse_make_bash_injection() {
    let content = "setup:\n\tmy_helper() { \\\n\t\techo \"setting up\"; \\\n\t}; \\\n\tmy_helper\n";
    let file = write_temp_file(content, "mk");
    let parser = Parser::new().unwrap();
    let (chunks, _calls, _types) = parser.parse_file_all(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"setup"),
        "Expected Make 'setup' rule, got: {:?}",
        names
    );
    // Bash injection may extract function if grammar can parse line-continued shell
}

#[test]
fn parse_make_pattern_rule() {
    let content = r#"
%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

install: all
	cp main /usr/local/bin/
"#;
    let file = write_temp_file(content, "mk");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"install"),
        "Expected 'install' rule, got: {:?}",
        names
    );
}

// -- nix ─────────────────────────────────────────────────────────────

#[test]
fn parse_nix_function_binding() {
    let content = r#"
{
  mkHello = name:
    "Hello, ${name}!";
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"mkHello"),
        "Expected 'mkHello', got: {:?}",
        names
    );
    let func = chunks.iter().find(|c| c.name == "mkHello").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_nix_attrset_binding() {
    let content = r#"
{
  config = {
    enableFeature = true;
    port = 8080;
  };
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"config"),
        "Expected 'config', got: {:?}",
        names
    );
    let cfg = chunks.iter().find(|c| c.name == "config").unwrap();
    assert_eq!(cfg.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_nix_calls() {
    let content = r#"
{
  myPackage = mkDerivation {
    name = "hello";
    buildInputs = [ pkgs.gcc ];
  };

  greet = name:
    builtins.trace "greeting" (lib.concatStrings ["Hello, " name]);
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // mkDerivation is called in myPackage binding
    let pkg = chunks.iter().find(|c| c.name == "myPackage");
    assert!(pkg.is_some(), "Expected 'myPackage' chunk");

    // Check calls in greet
    let greet = chunks.iter().find(|c| c.name == "greet");
    if let Some(g) = greet {
        let calls = parser.extract_calls_from_chunk(g);
        let callee_names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Should find builtins.trace or lib.concatStrings as qualified calls
        assert!(
            !callee_names.is_empty(),
            "Expected some calls in greet function"
        );
    }
}

// --- Injection tests ---

#[test]
fn parse_nix_shell_injection() {
    // buildPhase with bash content should trigger bash injection.
    // The outer binding `hello = mkDerivation { ... }` produces a Nix chunk.
    // The inner buildPhase indented string is injected as bash.
    let content = r#"
{
  hello = mkDerivation {
    name = "hello";
    buildPhase = ''
      mkdir -p build
      gcc -o build/hello src/main.c
    '';
    installPhase = ''
      mkdir -p $out/bin
      cp build/hello $out/bin/
    '';
  };
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Nix binding chunk should still exist
    assert!(
        chunks.iter().any(|c| c.language == Language::Nix),
        "Expected Nix chunks to survive injection, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_nix_non_shell_skipped() {
    // Indented strings NOT in shell contexts should be skipped
    let content = r#"
{
  description = ''
    This is a multi-line description.
    It should not be parsed as bash.
  '';
  longDescription = ''
    Another indented string that is just text,
    not shell code.
  '';
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // No bash chunks should be extracted
    let bash_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Bash)
        .collect();
    assert!(
        bash_chunks.is_empty(),
        "Non-shell indented strings should NOT produce bash chunks, got: {:?}",
        bash_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_nix_without_strings_unchanged() {
    // Nix file with no indented strings — injection should not fire
    let content = r#"
{
  add = a: b: a + b;
  config = {
    port = 8080;
  };
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // All chunks should be Nix
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Nix,
            "File without indented strings should only have Nix chunks"
        );
    }
}

#[test]
fn parse_nix_rec_attrset() {
    let content = r#"
{
  helpers = rec {
    double = x: x * 2;
    quadruple = x: double (double x);
  };
}
"#;
    let file = write_temp_file(content, "nix");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let helpers = chunks.iter().find(|c| c.name == "helpers");
    assert!(helpers.is_some(), "Expected 'helpers' chunk");
    assert_eq!(helpers.unwrap().chunk_type, ChunkType::Struct);
}

// -- objc ────────────────────────────────────────────────────────────

#[test]
fn parse_objc_class_interface() {
    let content = r#"
@interface Person : NSObject
@property (nonatomic) NSString *name;
- (void)greet;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Person").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_objc_protocol() {
    let content = r#"
@protocol Drawable
- (void)draw;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let proto = chunks.iter().find(|c| c.name == "Drawable").unwrap();
    assert_eq!(proto.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_objc_method_declaration() {
    let content = r#"
@interface Calculator : NSObject
- (int)add:(int)a to:(int)b;
+ (Calculator *)shared;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    let class_method = chunks.iter().find(|c| c.name == "shared").unwrap();
    assert_eq!(class_method.chunk_type, ChunkType::Method);
}

#[test]
fn parse_objc_method_definition() {
    let content = r#"
@implementation Person

- (void)greet {
    NSLog(@"Hello, %@", self.name);
}

@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

#[test]
fn parse_objc_free_function() {
    let content = "void freeFunc(int x) { }\n";
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "freeFunc").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_objc_property() {
    let content = r#"
@interface Config : NSObject
@property (nonatomic, copy) NSString *name;
@property (nonatomic) NSInteger count;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ptr_prop = chunks.iter().find(|c| c.name == "name").unwrap();
    assert_eq!(ptr_prop.chunk_type, ChunkType::Property);
    let val_prop = chunks.iter().find(|c| c.name == "count").unwrap();
    assert_eq!(val_prop.chunk_type, ChunkType::Property);
}

#[test]
fn parse_objc_calls() {
    let content = r#"
@implementation Runner

- (void)run {
    [self greet];
    NSLog(@"done");
    free(ptr);
}

@end
"#;
    let parser = Parser::new().unwrap();
    let lang = Language::ObjC;
    let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    // Message sends
    assert!(
        names.contains(&"greet"),
        "Expected greet call, got: {:?}",
        names
    );
    // C function calls
    assert!(
        names.contains(&"free"),
        "Expected free call, got: {:?}",
        names
    );
}

#[test]
fn parse_objc_category_interface() {
    let content = r#"
@interface NSString (Utilities)
- (BOOL)isBlank;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let cat = chunks.iter().find(|c| c.name == "NSString").unwrap();
    assert_eq!(cat.chunk_type, ChunkType::Extension);
}

#[test]
fn parse_objc_category_implementation() {
    let content = r#"
@implementation NSString (Utilities)

- (BOOL)isBlank {
    return [[self stringByTrimmingCharactersInSet:
        [NSCharacterSet whitespaceAndNewlineCharacterSet]] length] == 0;
}

@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // The implementation itself should be Extension
    let impls: Vec<_> = chunks
        .iter()
        .filter(|c| c.name == "NSString" && c.chunk_type == ChunkType::Extension)
        .collect();
    assert!(
        !impls.is_empty(),
        "Expected NSString category implementation as Extension, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_objc_regular_class_stays_class() {
    // Ensure non-category classes are still Class, not Extension
    let content = r#"
@interface Person : NSObject
@property (nonatomic) NSString *name;
@end
"#;
    let file = write_temp_file(content, "m");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Person").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

// -- ocaml ───────────────────────────────────────────────────────────

#[test]
fn parse_ocaml_function() {
    let content = r#"
let add x y = x + y
"#;
    let file = write_temp_file(content, "ml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_ocaml_type_variant() {
    let content = r#"
type color = Red | Green | Blue
"#;
    let file = write_temp_file(content, "ml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dt = chunks
        .iter()
        .find(|c| c.name == "color" && c.chunk_type == ChunkType::Enum);
    assert!(dt.is_some(), "Should find 'color' variant type as Enum");
}

#[test]
fn parse_ocaml_type_record() {
    let content = r#"
type point = {
  x : float;
  y : float;
}
"#;
    let file = write_temp_file(content, "ml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let dt = chunks
        .iter()
        .find(|c| c.name == "point" && c.chunk_type == ChunkType::Struct);
    assert!(dt.is_some(), "Should find 'point' record type as Struct");
}

#[test]
fn parse_ocaml_module() {
    let content = r#"
module Calculator = struct
  let add x y = x + y
end
"#;
    let file = write_temp_file(content, "ml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks
        .iter()
        .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
    assert!(module.is_some(), "Should find 'Calculator' module");
}

#[test]
fn parse_ocaml_calls() {
    let content = r#"
let process text =
  let trimmed = String.trim text in
  Printf.printf "%s\n" trimmed;
  validate trimmed
"#;
    let file = write_temp_file(content, "ml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"validate"),
        "Expected validate, got: {:?}",
        names
    );
}

// -- perl ────────────────────────────────────────────────────────────

#[test]
fn parse_perl_subroutine() {
    let content = r#"
sub add {
    my ($a, $b) = @_;
    return $a + $b;
}
"#;
    let file = write_temp_file(content, "pl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_perl_package() {
    let content = r#"
package Calculator;

sub add {
    my ($self, $a, $b) = @_;
    return $a + $b;
}

1;
"#;
    let file = write_temp_file(content, "pm");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let pkg = chunks
        .iter()
        .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
    assert!(pkg.is_some(), "Should find 'Calculator' package as Module");
}

#[test]
fn parse_perl_calls() {
    let content = r#"
sub process {
    my ($data) = @_;
    my $result = transform($data);
    validate($result);
    return $result;
}
"#;
    let file = write_temp_file(content, "pl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform, got: {:?}",
        names
    );
}

// -- php ─────────────────────────────────────────────────────────────

#[test]
fn parse_php_class() {
    let content = r#"<?php
class User {
    private string $name;
    public function getName(): string {
        return $this->name;
    }
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "User").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_php_interface() {
    let content = r#"<?php
interface Printable {
public function print(): void;
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
    assert_eq!(iface.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_php_trait() {
    let content = r#"<?php
trait Timestampable {
    public function getCreatedAt(): string {
        return date('Y-m-d');
    }
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let t = chunks.iter().find(|c| c.name == "Timestampable").unwrap();
    assert_eq!(t.chunk_type, ChunkType::Trait);
}

#[test]
fn parse_php_enum() {
    let content = r#"<?php
enum Status: string {
    case Active = 'active';
    case Inactive = 'inactive';
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Status").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_php_function() {
    let content = r#"<?php
function formatDuration(int $seconds): string {
    $hours = intdiv($seconds, 3600);
    return "{$hours}h";
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "formatDuration").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_php_method_in_class() {
    let content = r#"<?php
class Calculator {
    public function add(int $a, int $b): int {
        return $a + $b;
    }
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_php_constructor() {
    let content = r#"<?php
class User {
public function __construct(private string $name) {}
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks.iter().find(|c| c.name == "__construct").unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    assert_eq!(ctor.parent_type_name.as_deref(), Some("User"));
}

#[test]
fn parse_php_calls() {
    // NOTE: PHP grammar requires <?php tag, so extract_calls_from_chunk (which
    // re-parses chunk content without the tag) won't work. Use parse_file_calls
    // instead — this is the production path.
    let content = r#"<?php
function process(string $input): int {
$trimmed = trim($input);
$result = intval($trimmed);
echo $result;
return $result;
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let function_calls = parser.parse_file_calls(file.path()).unwrap();
    let func = function_calls
        .iter()
        .find(|fc| fc.name == "process")
        .unwrap();
    let names: Vec<_> = func.calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"trim"),
        "Expected trim call, got: {:?}",
        names
    );
    assert!(
        names.contains(&"intval"),
        "Expected intval call, got: {:?}",
        names
    );
}

#[test]
fn parse_php_property_strips_dollar() {
    let content = r#"<?php
class Config {
    public string $name = "default";
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let prop = chunks
        .iter()
        .find(|c| c.chunk_type == ChunkType::Property)
        .unwrap();
    assert_eq!(prop.name, "name", "Property name should have $ stripped");
}

// --- Multi-grammar injection tests ---

#[test]
fn parse_php_with_html_extracts_html_chunks() {
    // PHP template with HTML content between <?php blocks
    let content = r#"<?php
$title = "My Page";
?>
<!DOCTYPE html>
<html>
<body>
<h1><?php echo $title; ?></h1>
<nav id="main-nav">
  <a href="/">Home</a>
</nav>
</body>
</html>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should have HTML heading chunk
    let html_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Html)
        .collect();
    assert!(
        !html_chunks.is_empty(),
        "Expected HTML chunks from injection, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_php_with_html_script_extracts_js() {
    // PHP file with <script> in HTML region — 2-level chain: PHP→HTML→JS
    let content = r#"<?php
function getData(): array {
    return ['key' => 'value'];
}
?>
<html>
<body>
<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should have PHP function
    let php_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Php)
        .collect();
    assert!(
        php_chunks.iter().any(|c| c.name == "getData"),
        "Expected PHP function 'getData', got: {:?}",
        php_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    // Should have JS function (via recursive injection: PHP→HTML→JS)
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.iter().any(|c| c.name == "handleClick"),
        "Expected JS function 'handleClick' from 2-level injection, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_php_keeps_php_chunks() {
    // PHP functions/classes must survive injection processing
    let content = r#"<?php
class UserController {
    public function index(): string {
        return 'Hello';
    }
}
?>
<h1>Page Title</h1>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    assert!(
        chunks
            .iter()
            .any(|c| c.name == "UserController" && c.language == Language::Php),
        "PHP class 'UserController' should survive injection"
    );
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "index" && c.language == Language::Php),
        "PHP method 'index' should survive injection"
    );
}

#[test]
fn parse_php_without_html_unchanged() {
    // Pure PHP file (no text nodes) — injection should not fire
    let content = r#"<?php
function purePhp(): int {
    return 42;
}

class Standalone {
    public function method(): void {}
}
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // All chunks should be PHP
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Php,
            "Pure PHP file should have only PHP chunks, found {:?} for '{}'",
            chunk.language,
            chunk.name
        );
    }
    assert!(chunks.iter().any(|c| c.name == "purePhp"));
    assert!(chunks.iter().any(|c| c.name == "Standalone"));
}

#[test]
fn parse_php_interleaved() {
    // Interleaved PHP and HTML with embedded JS
    let content = r#"<?php echo "start"; ?>
<div>
<script>
function jsFunc() { return 1; }
</script>
</div>
<?php echo "end"; ?>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // JS function should be extracted
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.iter().any(|c| c.name == "jsFunc"),
        "Expected JS function 'jsFunc' from interleaved PHP, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_php_injection_call_graph() {
    // JS call graph should be extracted from PHP→HTML→JS
    let content = r#"<?php $x = 1; ?>
<script>
function caller() {
    helper();
}
function helper() {
    return 42;
}
</script>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

    let caller = calls.iter().find(|c| c.name == "caller");
    assert!(
        caller.is_some(),
        "Expected call graph for 'caller' from PHP→HTML→JS, got: {:?}",
        calls.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let callee_names: Vec<_> = caller
        .unwrap()
        .calls
        .iter()
        .map(|c| c.callee_name.as_str())
        .collect();
    assert!(
        callee_names.contains(&"helper"),
        "Expected caller→helper, got: {:?}",
        callee_names
    );
}

#[test]
fn parse_php_html_first() {
    // HTML before first <?php tag — `text` is a direct child of `program`
    let content = r#"<h1>Welcome</h1>
<nav id="main-nav">
  <a href="/">Home</a>
</nav>
<?php
function getTitle(): string {
    return "My Page";
}
?>
<footer>End</footer>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // PHP function should exist
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "getTitle" && c.language == Language::Php),
        "Expected PHP function 'getTitle'"
    );

    // HTML chunks should be extracted from both leading and trailing regions
    let html_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Html)
        .collect();
    assert!(
        !html_chunks.is_empty(),
        "Expected HTML chunks from file with leading HTML, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_php_injection_depth_limit() {
    // Verify that injection doesn't crash or produce garbage with normal PHP files.
    // The depth limit (MAX_INJECTION_DEPTH=3) should never be reached in practice
    // since PHP→HTML→JS is only depth 2. This test ensures the recursive machinery
    // handles the deepest real-world chain (PHP→HTML→JS/CSS) without issues.
    let content = r#"<?php
class App {
    public function render(): string {
        return '<html>';
    }
}
?>
<html>
<head>
<style>
body { color: red; }
.container { margin: 0 auto; }
</style>
</head>
<body>
<script>
function init() {
    document.querySelector('.container');
}
</script>
</body>
</html>
"#;
    let file = write_temp_file(content, "php");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Should have PHP, JS, and CSS chunks — full 3-level chain
    let php_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Php)
        .collect();
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    let css_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Css)
        .collect();

    assert!(!php_chunks.is_empty(), "Expected PHP chunks");
    assert!(
        js_chunks.iter().any(|c| c.name == "init"),
        "Expected JS function 'init' from PHP→HTML→JS chain, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
    assert!(
        !css_chunks.is_empty(),
        "Expected CSS chunks from PHP→HTML→CSS chain, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

// -- powershell ──────────────────────────────────────────────────────

#[test]
fn parse_powershell_function() {
    let content = r#"
function Get-UserInfo {
    param([string]$Name)
    Write-Output "Hello $Name"
}
"#;
    let file = write_temp_file(content, "ps1");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "Get-UserInfo").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_powershell_class() {
    let content = r#"
class Calculator {
    [int] Add([int]$a, [int]$b) {
        return $a + $b
    }
}
"#;
    let file = write_temp_file(content, "ps1");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_powershell_method() {
    let content = r#"
class Calculator {
[int] Add([int]$a, [int]$b) {
    return $a + $b
}
}
"#;
    let file = write_temp_file(content, "ps1");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "Add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_powershell_property() {
    let content = r#"
class Person {
[string]$Name
[int]$Age
}
"#;
    let file = write_temp_file(content, "ps1");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let prop = chunks
        .iter()
        .find(|c| c.name.contains("Name") && c.chunk_type == ChunkType::Property)
        .unwrap();
    assert_eq!(prop.chunk_type, ChunkType::Property);
}

#[test]
fn parse_powershell_enum() {
    let content = r#"
enum Color {
    Red
    Green
    Blue
}
"#;
    let file = write_temp_file(content, "ps1");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let en = chunks.iter().find(|c| c.name == "Color").unwrap();
    assert_eq!(en.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_powershell_calls() {
    let content = r#"
function Process-Data {
Get-Process -Name "foo"
$result = [System.IO.File]::ReadAllText("test.txt")
}
"#;
    let parser = Parser::new().unwrap();
    let lang = Language::PowerShell;
    let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"Get-Process"),
        "Expected Get-Process call, got: {:?}",
        names
    );
}

// -- protobuf ────────────────────────────────────────────────────────

#[test]
fn parse_proto_message() {
    let content = r#"
syntax = "proto3";

message User {
  string name = 1;
  int32 age = 2;
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let msg = chunks.iter().find(|c| c.name == "User").unwrap();
    assert_eq!(msg.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_proto_service() {
    let content = r#"
syntax = "proto3";

service UserService {
  rpc GetUser (GetUserRequest) returns (GetUserResponse);
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let svc = chunks
        .iter()
        .find(|c| c.name == "UserService" && c.chunk_type == ChunkType::Service);
    assert!(svc.is_some(), "Should find 'UserService' as Service");
}

#[test]
fn parse_proto_rpc() {
    let content = r#"
syntax = "proto3";

service UserService {
  rpc GetUser (GetUserRequest) returns (GetUserResponse);
  rpc ListUsers (ListUsersRequest) returns (stream User);
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let rpc = chunks.iter().find(|c| c.name == "GetUser").unwrap();
    assert_eq!(rpc.chunk_type, ChunkType::Method);
    assert_eq!(rpc.parent_type_name.as_deref(), Some("UserService"));
}

#[test]
fn parse_proto_enum() {
    let content = r#"
syntax = "proto3";

enum Status {
  UNKNOWN = 0;
  ACTIVE = 1;
  INACTIVE = 2;
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks
        .iter()
        .find(|c| c.name == "Status" && c.chunk_type == ChunkType::Enum);
    assert!(e.is_some(), "Should find 'Status' as Enum");
}

#[test]
fn parse_proto_calls() {
    let content = r#"
syntax = "proto3";

message User {
  string name = 1;
  Address address = 2;
}

message Address {
  string street = 1;
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let user = chunks.iter().find(|c| c.name == "User").unwrap();
    let calls = parser.extract_calls_from_chunk(user);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"Address"),
        "Expected Address type reference, got: {:?}",
        names
    );
}

// -- python ──────────────────────────────────────────────────────────

#[test]
fn parse_python_upper_case_constant() {
    let content = r#"
MAX_RETRIES = 3
API_URL = "https://example.com"
lowercase_var = 42
MixedCase = "nope"

def foo():
    pass
"#;
    let file = write_temp_file(content, "py");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(max.is_some(), "Should capture MAX_RETRIES");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
    let url = chunks.iter().find(|c| c.name == "API_URL");
    assert!(url.is_some(), "Should capture API_URL");
    assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
    // lowercase and MixedCase are now captured as Variable (not Constant)
    let lc = chunks.iter().find(|c| c.name == "lowercase_var");
    assert!(lc.is_some(), "Should capture lowercase_var as Variable");
    assert_eq!(lc.unwrap().chunk_type, ChunkType::Variable);
    let mc = chunks.iter().find(|c| c.name == "MixedCase");
    assert!(mc.is_some(), "Should capture MixedCase as Variable");
    assert_eq!(mc.unwrap().chunk_type, ChunkType::Variable);
}

#[test]
fn parse_python_constructor() {
    let content = r#"
class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        print(self.name)
"#;
    let file = write_temp_file(content, "py");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks.iter().find(|c| c.name == "__init__").unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    // greet should still be a Method
    let method = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

// -- r ───────────────────────────────────────────────────────────────

#[test]
fn parse_r_function_arrow() {
    let content = r#"
greet <- function(name) {
    paste("Hello,", name)
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_r_function_equals() {
    let content = r#"
greet = function(name) {
    paste("Hello,", name)
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_r_multiple_functions() {
    let content = r#"
add <- function(a, b) {
    a + b
}

multiply <- function(a, b) {
    a * b
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(chunks.iter().any(|c| c.name == "add"));
    assert!(chunks.iter().any(|c| c.name == "multiply"));
}

#[test]
fn parse_r_calls() {
    let content = r#"
process_data <- function(df) {
    cleaned <- na.omit(df)
    result <- mean(cleaned$value)
    print(result)
    return(result)
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process_data").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(names.contains(&"print"), "Expected print, got: {:?}", names);
}

#[test]
fn parse_r_s4_set_class() {
    let content = r#"
setClass("Person",
  representation(
    name = "character",
    age = "numeric"
  )
)
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Person");
    assert!(class.is_some(), "Should capture S4 class 'Person'");
    assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
}

#[test]
fn parse_r_s4_set_ref_class() {
    let content = r#"
setRefClass("Counter",
  fields = list(
    count = "numeric"
  ),
  methods = list(
    increment = function() {
      count <<- count + 1
    }
  )
)
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Counter");
    assert!(
        class.is_some(),
        "Should capture S4 reference class 'Counter'"
    );
    assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
}

#[test]
fn parse_r_non_class_call_filtered() {
    // A non-class-defining call like library() should not be captured
    let content = r#"
library(ggplot2)
print("hello")
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks.is_empty(),
        "Non-class calls should be filtered out, got: {:?}",
        chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

// --- R6 class tests ---

#[test]
fn parse_r_r6_class() {
    let content = r#"
Person <- R6Class("Person",
  public = list(
    name = NULL,
    initialize = function(name) {
      self$name <- name
    },
    greet = function() {
      cat(paste0("Hello, my name is ", self$name, ".\n"))
    }
  )
)
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Person");
    assert!(class.is_some(), "Should capture R6 class 'Person'");
    assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
}

#[test]
fn parse_r_r6_class_equals() {
    let content = r#"
Animal = R6Class("Animal",
  public = list(
    species = NULL,
    initialize = function(species) {
      self$species <- species
    }
  )
)
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Animal");
    assert!(class.is_some(), "Should capture R6 class with = assignment");
    assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
}

// --- Constant tests ---

#[test]
fn parse_r_upper_case_constants() {
    let content = r#"
MAX_RETRIES <- 3
API_URL <- "https://example.com"
DEFAULT_TIMEOUT <- 30
lowercase_var <- 42
MixedCase <- "nope"

my_func <- function(x) { x }
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(max.is_some(), "Should capture MAX_RETRIES");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);

    let url = chunks.iter().find(|c| c.name == "API_URL");
    assert!(url.is_some(), "Should capture API_URL");
    assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);

    let timeout = chunks.iter().find(|c| c.name == "DEFAULT_TIMEOUT");
    assert!(timeout.is_some(), "Should capture DEFAULT_TIMEOUT");
    assert_eq!(timeout.unwrap().chunk_type, ChunkType::Constant);

    // lowercase and MixedCase should be filtered out
    assert!(
        chunks.iter().find(|c| c.name == "lowercase_var").is_none(),
        "Should not capture lowercase_var"
    );
    assert!(
        chunks.iter().find(|c| c.name == "MixedCase").is_none(),
        "Should not capture MixedCase"
    );

    // Function should still be captured
    let func = chunks.iter().find(|c| c.name == "my_func");
    assert!(func.is_some(), "Should still capture functions");
    assert_eq!(func.unwrap().chunk_type, ChunkType::Function);
}

#[test]
fn parse_r_constants_with_equals() {
    let content = r#"
MAX_VAL = 100
API_KEY = "secret"
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let max = chunks.iter().find(|c| c.name == "MAX_VAL");
    assert!(max.is_some(), "Should capture MAX_VAL with = assignment");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
}

#[test]
fn parse_r_constants_inside_function_ignored() {
    let content = r#"
my_func <- function() {
    MAX_LOCAL <- 99
    result <- MAX_LOCAL + 1
    return(result)
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Only the function should be captured, not the local constant
    assert_eq!(chunks.len(), 1, "Should only capture the function");
    assert_eq!(chunks[0].name, "my_func");
    assert_eq!(chunks[0].chunk_type, ChunkType::Function);
}

#[test]
fn parse_r_boolean_and_special_constants() {
    let content = r#"
USE_CACHE <- TRUE
EMPTY_VAL <- NULL
MISSING <- NA
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let cache = chunks.iter().find(|c| c.name == "USE_CACHE");
    assert!(cache.is_some(), "Should capture TRUE constant");
    assert_eq!(cache.unwrap().chunk_type, ChunkType::Constant);

    let empty = chunks.iter().find(|c| c.name == "EMPTY_VAL");
    assert!(empty.is_some(), "Should capture NULL constant");
    assert_eq!(empty.unwrap().chunk_type, ChunkType::Constant);

    let missing = chunks.iter().find(|c| c.name == "MISSING");
    assert!(missing.is_some(), "Should capture NA constant");
    assert_eq!(missing.unwrap().chunk_type, ChunkType::Constant);
}

// --- Mixed file test ---

#[test]
fn parse_r_mixed_file() {
    let content = r#"
#' @title Person class
setClass("Person",
  representation(name = "character", age = "numeric")
)

Logger <- R6Class("Logger",
  public = list(
    log = function(msg) cat(msg, "\n")
  )
)

MAX_CONNECTIONS <- 100

process <- function(x) {
    x * 2
}
"#;
    let file = write_temp_file(content, "r");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<(&str, ChunkType)> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names.contains(&("Person", ChunkType::Class)),
        "Should have S4 class Person, got: {:?}",
        names
    );
    assert!(
        names.contains(&("Logger", ChunkType::Class)),
        "Should have R6 class Logger, got: {:?}",
        names
    );
    assert!(
        names.contains(&("MAX_CONNECTIONS", ChunkType::Constant)),
        "Should have constant MAX_CONNECTIONS, got: {:?}",
        names
    );
    assert!(
        names.contains(&("process", ChunkType::Function)),
        "Should have function process, got: {:?}",
        names
    );
}

// -- razor ───────────────────────────────────────────────────────────

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
    let ctor = chunks
        .iter()
        .find(|c| c.name == "MyService" && c.chunk_type == ChunkType::Constructor);
    assert!(
        ctor.is_some(),
        "Expected 'MyService' constructor as Constructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

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
    let handle = function_calls.iter().find(|fc| fc.name == "HandleClick");
    assert!(handle.is_some(), "Expected 'HandleClick' in call graph");
    let handle = handle.unwrap();
    let callee_names: Vec<_> = handle
        .calls
        .iter()
        .map(|c| c.callee_name.as_str())
        .collect();
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
        names
            .iter()
            .any(|(n, t)| *n == "Logger" && *t == ChunkType::Property),
        "Expected 'Logger' inject property, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Counter" && *t == ChunkType::Section),
        "Expected 'Counter' heading section, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "IncrementCount" && *t == ChunkType::Method),
        "Expected 'IncrementCount' method (inside razor_block container), got: {:?}",
        names
    );
}

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
        .filter(|c| c.language == Language::JavaScript)
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
        .filter(|c| c.language == Language::Css)
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
            Language::Razor,
            "File without script/style should only have Razor chunks, got {:?} for '{}'",
            chunk.language,
            chunk.name
        );
    }
}

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

// -- ruby ────────────────────────────────────────────────────────────

#[test]
fn parse_ruby_class() {
    let content = r#"
class Calculator
  def add(a, b)
    a + b
  end
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_ruby_module() {
    let content = r#"
module Helpers
  def helper
42
  end
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let module = chunks.iter().find(|c| c.name == "Helpers").unwrap();
    assert_eq!(module.chunk_type, ChunkType::Module);
}

#[test]
fn parse_ruby_method() {
    let content = r#"
def standalone_method(x)
  x * 2
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks
        .iter()
        .find(|c| c.name == "standalone_method")
        .unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_ruby_singleton_method() {
    let content = r#"
class Foo
  def self.bar
    "hello"
  end
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "bar").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

#[test]
fn parse_ruby_method_in_class() {
    let content = r#"
class Calculator
  def add(a, b)
a + b
  end
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_ruby_method_in_module() {
    let content = r#"
module StringUtils
  def capitalize_all(str)
str.upcase
  end
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "capitalize_all").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("StringUtils"));
}

#[test]
fn parse_ruby_constant() {
    let content = "MAX_RETRIES = 3\n";
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let c = chunks.iter().find(|c| c.name == "MAX_RETRIES").unwrap();
    assert_eq!(c.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_ruby_calls() {
    let content = r#"
def process(input)
  result = transform(input)
  result.to_s
  puts(result)
end
"#;
    let file = write_temp_file(content, "rb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform call, got: {:?}",
        names
    );
    assert!(
        names.contains(&"puts"),
        "Expected puts call, got: {:?}",
        names
    );
}

// -- rust ────────────────────────────────────────────────────────────

#[test]
fn parse_rust_type_alias() {
    let content = "type Result<T> = std::result::Result<T, MyError>;\n";
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "Result").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_rust_constructor() {
    let content = r#"
struct Config {
    path: String,
}

impl Config {
    fn new(path: String) -> Self {
        Config { path }
    }

    fn validate(&self) -> bool {
        true
    }
}
"#;
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks.iter().find(|c| c.name == "new").unwrap();
    assert_eq!(ctor.chunk_type, ChunkType::Constructor);
    // validate should still be a Method
    let method = chunks.iter().find(|c| c.name == "validate").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

// -- scala ───────────────────────────────────────────────────────────

#[test]
fn parse_scala_class() {
    let content = r#"
class Calculator {
  def add(a: Int, b: Int): Int = {
    a + b
  }
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_scala_object() {
    let content = r#"
object Main {
  def run(): Unit = {
println("hello")
  }
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let obj = chunks.iter().find(|c| c.name == "Main").unwrap();
    assert_eq!(obj.chunk_type, ChunkType::Object);
}

#[test]
fn parse_scala_trait() {
    let content = r#"
trait Printable {
  def print(): Unit
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let t = chunks.iter().find(|c| c.name == "Printable").unwrap();
    assert_eq!(t.chunk_type, ChunkType::Trait);
}

#[test]
fn parse_scala_type_alias() {
    let content = "type StringMap = Map[String, String]\n";
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_scala_method_in_class() {
    let content = r#"
class Calculator {
  def add(a: Int, b: Int): Int = {
    a + b
  }
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_scala_val_const() {
    let content = r#"
object Config {
  val maxRetries: Int = 3
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let val_chunk = chunks.iter().find(|c| c.name == "maxRetries").unwrap();
    assert_eq!(val_chunk.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_scala_calls() {
    let content = r#"
object App {
  def process(input: String): Unit = {
    val result = transform(input)
    println(result.toString)
  }
}
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform, got: {:?}",
        names
    );
    assert!(
        names.contains(&"println"),
        "Expected println, got: {:?}",
        names
    );
}

#[test]
fn parse_scala3_extension() {
    let content = r#"
extension (x: Int)
  def isEven: Boolean = x % 2 == 0
  def double: Int = x * 2
"#;
    let file = write_temp_file(content, "scala");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ext = chunks.iter().find(|c| c.chunk_type == ChunkType::Extension);
    assert!(
        ext.is_some(),
        "Expected an extension chunk, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    let ext = ext.unwrap();
    assert_eq!(ext.name, "Int");
    assert_eq!(ext.chunk_type, ChunkType::Extension);
}

// -- solidity ────────────────────────────────────────────────────────

#[test]
fn parse_solidity_contract() {
    let content = r#"
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract Token {
    string public name;
    uint256 public totalSupply;

    function transfer(address to, uint256 amount) public returns (bool) {
        return true;
    }
}
"#;
    let file = write_temp_file(content, "sol");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let contract = chunks.iter().find(|c| c.name == "Token").unwrap();
    assert_eq!(contract.chunk_type, ChunkType::Class);
    let func = chunks.iter().find(|c| c.name == "transfer").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Method);
    assert_eq!(func.parent_type_name.as_deref(), Some("Token"));
}

#[test]
fn parse_solidity_interface() {
    let content = r#"
interface IERC20 {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
}
"#;
    let file = write_temp_file(content, "sol");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let iface = chunks.iter().find(|c| c.name == "IERC20").unwrap();
    assert_eq!(iface.chunk_type, ChunkType::Interface);
}

#[test]
fn parse_solidity_calls() {
    let content = r#"
contract Caller {
    function doWork() public {
        token.transfer(msg.sender, 100);
        require(true, "failed");
    }
}
"#;
    let file = write_temp_file(content, "sol");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "doWork").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transfer"),
        "Expected transfer, got: {:?}",
        names
    );
    assert!(
        names.contains(&"require"),
        "Expected require, got: {:?}",
        names
    );
}

#[test]
fn parse_solidity_struct_and_enum() {
    let content = r#"
struct Position {
    uint256 x;
    uint256 y;
}

enum Status { Active, Paused, Stopped }
"#;
    let file = write_temp_file(content, "sol");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Position").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
    let e = chunks.iter().find(|c| c.name == "Status").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_solidity_event() {
    let content = r#"
contract Token {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
}
"#;
    let file = write_temp_file(content, "sol");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let transfer = chunks.iter().find(|c| c.name == "Transfer").unwrap();
    assert_eq!(transfer.chunk_type, ChunkType::Event);
    let approval = chunks.iter().find(|c| c.name == "Approval").unwrap();
    assert_eq!(approval.chunk_type, ChunkType::Event);
}

// -- sql ─────────────────────────────────────────────────────────────

#[test]
fn parse_sql_create_table() {
    let content = "CREATE TABLE users (\n  id INT PRIMARY KEY,\n  name VARCHAR(100)\n);\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let table = chunks.iter().find(|c| c.name == "users").unwrap();
    assert_eq!(table.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_sql_create_view_as_function() {
    let content = "CREATE VIEW active_users AS\nSELECT * FROM users WHERE active = 1;\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let view = chunks.iter().find(|c| c.name == "active_users").unwrap();
    assert_eq!(view.chunk_type, ChunkType::StoredProc);
}

#[test]
fn parse_sql_stored_procedure() {
    let content =
        "CREATE PROCEDURE update_salary(emp_id INT, new_salary DECIMAL)\nBEGIN\n  UPDATE employees SET salary = new_salary WHERE id = emp_id;\nEND;\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let proc = chunks.iter().find(|c| c.name == "update_salary").unwrap();
    assert_eq!(proc.chunk_type, ChunkType::StoredProc);
}

#[test]
fn parse_sql_trigger() {
    // tree-sitter-sql doesn't have a create_trigger node — triggers are not parsed.
    // The @storedproc capture is ready in the .scm file for when the grammar adds support.
    // tree-sitter-sequel-tsql parses triggers but the name extraction pattern
    // is grammar-version-dependent. Test that we at least parse without crashing
    // and find a StoredProc chunk if the grammar supports it.
    let content =
        "CREATE TRIGGER audit_insert AFTER INSERT ON orders\nBEGIN\n  INSERT INTO audit_log VALUES (NEW.id);\nEND;\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    if let Some(trigger) = chunks.iter().find(|c| c.name.contains("audit_insert")) {
        assert_eq!(trigger.chunk_type, ChunkType::StoredProc);
    }
    // No panic = grammar handles the syntax
}

#[test]
fn parse_sql_function_stays_function() {
    let content =
        "CREATE FUNCTION add_numbers(a INT, b INT) RETURNS INT\nBEGIN\n  RETURN a + b;\nEND;\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add_numbers").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_rust_test_function() {
    let content = r#"
#[test]
fn test_addition() {
    assert_eq!(2 + 2, 4);
}

fn regular_function() {
    println!("not a test");
}
"#;
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let test_fn = chunks.iter().find(|c| c.name == "test_addition").unwrap();
    assert_eq!(test_fn.chunk_type, ChunkType::Test);
    let regular = chunks
        .iter()
        .find(|c| c.name == "regular_function")
        .unwrap();
    assert_eq!(regular.chunk_type, ChunkType::Function);
}

#[test]
fn parse_rust_static_mut_is_variable() {
    let content = r#"
static mut COUNTER: u32 = 0;
static IMMUTABLE: &str = "hello";
const MAX: u32 = 100;
"#;
    let file = write_temp_file(content, "rs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let counter = chunks.iter().find(|c| c.name == "COUNTER").unwrap();
    assert_eq!(counter.chunk_type, ChunkType::Variable);
    let immut = chunks.iter().find(|c| c.name == "IMMUTABLE").unwrap();
    assert_eq!(immut.chunk_type, ChunkType::Constant);
    let max_c = chunks.iter().find(|c| c.name == "MAX").unwrap();
    assert_eq!(max_c.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_python_test_function() {
    let content = r#"
def test_login():
    assert True

def helper_function():
    pass
"#;
    let file = write_temp_file(content, "py");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let test_fn = chunks.iter().find(|c| c.name == "test_login").unwrap();
    assert_eq!(test_fn.chunk_type, ChunkType::Test);
    let helper = chunks.iter().find(|c| c.name == "helper_function").unwrap();
    assert_eq!(helper.chunk_type, ChunkType::Function);
}

#[test]
fn parse_go_test_function() {
    let content = r#"
package main

func TestAdd(t *testing.T) {
    if add(1, 2) != 3 {
        t.Error("expected 3")
    }
}

func add(a, b int) int {
    return a + b
}
"#;
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let test_fn = chunks.iter().find(|c| c.name == "TestAdd").unwrap();
    assert_eq!(test_fn.chunk_type, ChunkType::Test);
    let add_fn = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(add_fn.chunk_type, ChunkType::Function);
}

#[test]
fn parse_go_var_declaration() {
    let content = r#"
package main

var globalCount int = 0
const MaxRetries = 3
"#;
    let file = write_temp_file(content, "go");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let var = chunks.iter().find(|c| c.name == "globalCount").unwrap();
    assert_eq!(var.chunk_type, ChunkType::Variable);
    let cnst = chunks.iter().find(|c| c.name == "MaxRetries").unwrap();
    assert_eq!(cnst.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_javascript_let_var_declarations() {
    let content = r#"
let counter = 0;
var legacy = "old";
const MAX = 100;
"#;
    let file = write_temp_file(content, "js");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let let_var = chunks.iter().find(|c| c.name == "counter").unwrap();
    assert_eq!(let_var.chunk_type, ChunkType::Variable);
    let var_var = chunks.iter().find(|c| c.name == "legacy").unwrap();
    assert_eq!(var_var.chunk_type, ChunkType::Variable);
    let const_var = chunks.iter().find(|c| c.name == "MAX").unwrap();
    assert_eq!(const_var.chunk_type, ChunkType::Constant);
}

#[test]
fn parse_protobuf_service() {
    let content = r#"
syntax = "proto3";
service UserService {
  rpc GetUser (GetUserRequest) returns (User);
  rpc CreateUser (CreateUserRequest) returns (User);
}
message User {
  string name = 1;
}
"#;
    let file = write_temp_file(content, "proto");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let svc = chunks.iter().find(|c| c.name == "UserService").unwrap();
    assert_eq!(svc.chunk_type, ChunkType::Service);
    let msg = chunks.iter().find(|c| c.name == "User").unwrap();
    assert_eq!(msg.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_sql_create_type() {
    let content = "CREATE TYPE status AS ENUM ('active', 'inactive');\n";
    let file = write_temp_file(content, "sql");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ty = chunks.iter().find(|c| c.name == "status").unwrap();
    assert_eq!(ty.chunk_type, ChunkType::TypeAlias);
}

// -- structured_text ─────────────────────────────────────────────────

#[test]
fn parse_function_block() {
    let content = r#"
FUNCTION_BLOCK PID_Controller
VAR_INPUT
    SetPoint : REAL;
    ProcessValue : REAL;
END_VAR
VAR_OUTPUT
    Output : REAL;
END_VAR
    Output := SetPoint - ProcessValue;
END_FUNCTION_BLOCK
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let fb = chunks.iter().find(|c| c.name == "PID_Controller").unwrap();
    assert_eq!(fb.chunk_type, ChunkType::Class);
}

#[test]
fn parse_function() {
    let content = r#"
FUNCTION CalculateChecksum : INT
VAR_INPUT
Length : INT;
END_VAR
VAR
Sum : INT;
END_VAR
Sum := 0;
CalculateChecksum := Sum;
END_FUNCTION
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let f = chunks
        .iter()
        .find(|c| c.name == "CalculateChecksum")
        .unwrap();
    assert_eq!(f.chunk_type, ChunkType::Function);
}

#[test]
fn parse_program() {
    let content = r#"
PROGRAM Main
VAR
    Temperature : REAL;
END_VAR
    Temperature := 72.5;
END_PROGRAM
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let p = chunks.iter().find(|c| c.name == "Main").unwrap();
    assert_eq!(p.chunk_type, ChunkType::Module);
}

#[test]
fn parse_type_definition() {
    let content = r#"
TYPE MotorState :
STRUCT
    Speed : REAL;
    Running : BOOL;
    Direction : INT;
END_STRUCT;
END_TYPE
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let t = chunks.iter().find(|c| c.name == "MotorState").unwrap();
    assert_eq!(t.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_call_graph() {
    let content = r#"
PROGRAM Main
VAR
    PID1 : PID_Controller;
    Temp : REAL;
END_VAR
    PID1(SetPoint := 72.5, ProcessValue := Temp);
END_PROGRAM
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let main = chunks.iter().find(|c| c.name == "Main").unwrap();
    // PID1 call should be in the chunk content
    assert!(main.content.contains("PID1"));
}

#[test]
fn parse_method_definition() {
    let content = r#"
FUNCTION_BLOCK Motor
METHOD PUBLIC Start : BOOL
VAR_INPUT
    Speed : REAL;
END_VAR
    Start := Speed > 0.0;
END_METHOD
END_FUNCTION_BLOCK
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "Start").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

#[test]
fn parse_action_definition() {
    let content = r#"
FUNCTION_BLOCK Controller
ACTION ResetCounters
    Counter1 := 0;
    Counter2 := 0;
END_ACTION
END_FUNCTION_BLOCK
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let action = chunks.iter().find(|c| c.name == "ResetCounters").unwrap();
    // ACTION inside FUNCTION_BLOCK is treated as a method (parent container inference)
    assert_eq!(action.chunk_type, ChunkType::Method);
}

#[test]
fn parse_type_references_in_var_input() {
    let content = r#"
FUNCTION_BLOCK Conveyor
VAR_INPUT
    Speed : REAL;
    Sensor : ProximitySensor;
END_VAR
    (* control logic *)
END_FUNCTION_BLOCK
"#;
    let file = write_temp_file(content, "st");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let fb = chunks.iter().find(|c| c.name == "Conveyor").unwrap();
    assert_eq!(fb.chunk_type, ChunkType::Class);
    // Typed VAR_INPUT declarations should be in chunk content
    assert!(fb.content.contains("ProximitySensor"));
}

// -- svelte ──────────────────────────────────────────────────────────

#[test]
fn parse_svelte_with_script() {
    let content = r#"<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function formatName(first, last) {
    return `${first} ${last}`;
}
</script>

<h1>Hello World</h1>
<button on:click={handleClick}>Click me</button>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // JS functions should be extracted via injection
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.iter().any(|c| c.name == "handleClick"),
        "Expected JS function 'handleClick', got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
    assert!(
        js_chunks.iter().any(|c| c.name == "formatName"),
        "Expected JS function 'formatName'"
    );
}

#[test]
fn parse_svelte_with_typescript() {
    let content = r#"<script lang="ts">
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello, ${user.name}!`;
}
</script>

<p>Content</p>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // TypeScript should be detected via lang="ts"
    let ts_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::TypeScript)
        .collect();
    assert!(
        ts_chunks.iter().any(|c| c.name == "greet"),
        "Expected TS function 'greet' from <script lang=\"ts\">, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_svelte_with_style() {
    let content = r#"<script>
function init() { return 1; }
</script>

<div class="container">Hello</div>

<style>
.container {
    max-width: 1200px;
    margin: 0 auto;
}

body {
    font-family: sans-serif;
}
</style>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // CSS chunks from style block
    let css_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Css)
        .collect();
    assert!(
        !css_chunks.is_empty(),
        "Expected CSS chunks from <style> block, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_svelte_template_elements() {
    let content = r#"<script>
let count = 0;
</script>

<h1>Counter App</h1>

<nav id="main-nav">
  <a href="/">Home</a>
  <a href="/about">About</a>
</nav>

<main>
  <p>Count: {count}</p>
</main>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // Heading should be extracted as Section
    let sections: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Section)
        .collect();
    assert!(
        sections.iter().any(|c| c.name == "Counter App"),
        "Expected heading 'Counter App' as Section, got: {:?}",
        sections.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    // Nav landmark
    assert!(
        sections.iter().any(|c| c.name == "nav#main-nav"),
        "Expected nav landmark, got: {:?}",
        sections.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_svelte_call_graph() {
    let content = r#"<script>
function fetchData() {
    return fetch('/api/data');
}

function handleSubmit(event) {
    const data = fetchData();
    process(data);
}
</script>

<button on:click={handleSubmit}>Submit</button>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

    let handler = calls.iter().find(|c| c.name == "handleSubmit");
    assert!(
        handler.is_some(),
        "Expected call graph for 'handleSubmit', got: {:?}",
        calls.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let callee_names: Vec<_> = handler
        .unwrap()
        .calls
        .iter()
        .map(|c| c.callee_name.as_str())
        .collect();
    assert!(
        callee_names.contains(&"fetchData"),
        "Expected handleSubmit→fetchData, got: {:?}",
        callee_names
    );
}

#[test]
fn parse_svelte_no_script_unchanged() {
    // Template-only Svelte component — no injection should fire
    let content = r#"<h1>Simple Page</h1>
<nav id="sidebar">
  <ul>
    <li><a href="/">Home</a></li>
  </ul>
</nav>
<main>
  <p>Hello world</p>
</main>
"#;
    let file = write_temp_file(content, "svelte");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // All chunks should be Svelte (no JS/CSS)
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Svelte,
            "Template-only Svelte should only have Svelte chunks, found {:?} for '{}'",
            chunk.language,
            chunk.name
        );
    }

    assert!(
        chunks.iter().any(|c| c.name == "Simple Page"),
        "Expected heading 'Simple Page'"
    );
}

// -- swift ───────────────────────────────────────────────────────────

#[test]
fn parse_swift_class() {
    let content = r#"
class Shape {
    var sides: Int = 0

    func describe() -> String {
        return "A shape with \(sides) sides"
    }
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let class = chunks.iter().find(|c| c.name == "Shape").unwrap();
    assert_eq!(class.chunk_type, ChunkType::Class);
}

#[test]
fn parse_swift_struct() {
    let content = r#"
struct Point {
    var x: Double
    var y: Double
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Point").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_swift_enum() {
    let content = r#"
enum Direction {
    case north
    case south
    case east
    case west
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Direction").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_swift_protocol() {
    let content = r#"
protocol Drawable {
    func draw()
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let p = chunks.iter().find(|c| c.name == "Drawable").unwrap();
    assert_eq!(p.chunk_type, ChunkType::Trait);
}

#[test]
fn parse_swift_function() {
    let content = r#"
func greet(name: String) -> String {
    return "Hello, \(name)!"
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_swift_actor() {
    let content = r#"
actor Counter {
    var count: Int = 0

    func increment() {
        count += 1
    }
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let a = chunks.iter().find(|c| c.name == "Counter").unwrap();
    assert_eq!(a.chunk_type, ChunkType::Class);
}

#[test]
fn parse_swift_extension() {
    let content = r#"
struct Point {
var x: Double
var y: Double
}

extension Point {
func distance() -> Double {
    return (x * x + y * y).squareRoot()
}
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Both the struct and the extension should have name "Point"
    let point_chunks: Vec<_> = chunks.iter().filter(|c| c.name == "Point").collect();
    assert!(
        point_chunks.len() >= 2,
        "Expected at least 2 Point chunks (struct + extension), got: {}",
        point_chunks.len()
    );
    // The struct should be Struct type
    assert!(
        point_chunks
            .iter()
            .any(|c| c.chunk_type == ChunkType::Struct),
        "Expected one Point to be Struct"
    );
    // The extension should be Extension type
    assert!(
        point_chunks
            .iter()
            .any(|c| c.chunk_type == ChunkType::Extension),
        "Expected one Point to be Extension"
    );
}

#[test]
fn parse_swift_typealias() {
    let content = "typealias StringMap = Dictionary<String, String>\n";
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_swift_calls() {
    let content = r#"
func process(input: String) -> Int {
    let trimmed = input.trimmingCharacters(in: .whitespaces)
    let result = transform(trimmed)
    print(result)
    return result.count
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"transform"),
        "Expected transform call, got: {:?}",
        names
    );
    assert!(
        names.contains(&"print"),
        "Expected print call, got: {:?}",
        names
    );
}

#[test]
fn parse_swift_method_in_class() {
    let content = r#"
class Calculator {
    func add(a: Int, b: Int) -> Int {
        return a + b
    }
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
    assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
}

#[test]
fn parse_swift_constructor() {
    let content = r#"
class Server {
let port: Int

init(port: Int) {
    self.port = port
}

func start() { }
}
"#;
    let file = write_temp_file(content, "swift");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ctor = chunks
        .iter()
        .find(|c| c.name == "init" && c.chunk_type == ChunkType::Constructor);
    assert!(
        ctor.is_some(),
        "Expected init as Constructor, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, c.chunk_type))
            .collect::<Vec<_>>()
    );
    // start should still be a Method
    let method = chunks.iter().find(|c| c.name == "start").unwrap();
    assert_eq!(method.chunk_type, ChunkType::Method);
}

// -- toml_lang ───────────────────────────────────────────────────────

#[test]
fn parse_toml_table() {
    let content = r#"
[package]
name = "my-crate"
version = "1.0.0"

[dependencies]
serde = "1.0"
"#;
    let file = write_temp_file(content, "toml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"package"),
        "Expected 'package' table, got: {:?}",
        names
    );
    assert!(
        names.contains(&"dependencies"),
        "Expected 'dependencies' table, got: {:?}",
        names
    );
}

#[test]
fn parse_toml_chunk_type() {
    let content = r#"
[server]
host = "localhost"
port = 8080
"#;
    let file = write_temp_file(content, "toml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let server = chunks.iter().find(|c| c.name == "server");
    assert!(server.is_some(), "Expected 'server' chunk");
    assert_eq!(server.unwrap().chunk_type, ChunkType::ConfigKey);
}

#[test]
fn parse_toml_no_calls() {
    let content = r#"
[database]
host = "localhost"
port = 5432
"#;
    let file = write_temp_file(content, "toml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "TOML should have no calls");
    }
}

// -- typescript ──────────────────────────────────────────────────────

#[test]
fn parse_typescript_namespace() {
    let content = "namespace Validators {\n  export function check() {}\n}\n";
    let file = write_temp_file(content, "ts");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ns = chunks.iter().find(|c| c.name == "Validators").unwrap();
    assert_eq!(ns.chunk_type, ChunkType::Module);
}

#[test]
fn parse_typescript_type_alias() {
    let content = "type Result = Success | Failure;\n";
    let file = write_temp_file(content, "ts");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let ta = chunks.iter().find(|c| c.name == "Result").unwrap();
    assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
}

#[test]
fn parse_typescript_const_value() {
    let content = r#"
const MAX_RETRIES: number = 3;
const API_URL = "https://example.com";
const handler = () => { return 1; };

function foo() {}
"#;
    let file = write_temp_file(content, "ts");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(max.is_some(), "Should capture MAX_RETRIES");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
    let url = chunks.iter().find(|c| c.name == "API_URL");
    assert!(url.is_some(), "Should capture API_URL");
    assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
    // handler is an arrow function — should be Function, not Constant
    let handler = chunks.iter().find(|c| c.name == "handler");
    assert!(handler.is_some(), "Should capture handler");
    assert_eq!(handler.unwrap().chunk_type, ChunkType::Function);
}

// -- vbnet ───────────────────────────────────────────────────────────

#[test]
fn parse_vbnet_class_with_methods() {
    let content = r#"
Public Class Calculator
    Private _value As Integer

    Public Sub New()
        _value = 0
    End Sub

    Public Function Add(a As Integer, b As Integer) As Integer
        Return a + b
    End Function

    Public Sub Reset()
        _value = 0
    End Sub
End Class
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Calculator" && *t == ChunkType::Class),
        "Expected 'Calculator' class, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "New" && *t == ChunkType::Constructor),
        "Expected 'New' constructor, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Add" && *t == ChunkType::Method),
        "Expected 'Add' method, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Reset" && *t == ChunkType::Method),
        "Expected 'Reset' method, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_module() {
    let content = r#"
Module Program
    Sub Main()
        Console.WriteLine("Hello")
    End Sub

    Function Add(a As Integer, b As Integer) As Integer
        Return a + b
    End Function
End Module
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();

    assert!(
        names.contains(&"Program"),
        "Expected 'Program' module, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Main"),
        "Expected 'Main' method, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Add"),
        "Expected 'Add' method, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_interface() {
    let content = r#"
Public Interface IPayable
    Function CalculatePay() As Decimal
    ReadOnly Property Name As String
End Interface
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "IPayable" && *t == ChunkType::Interface),
        "Expected 'IPayable' interface, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_structure() {
    let content = r#"
Public Structure Point
    Public X As Double
    Public Y As Double

    Public Sub New(x As Double, y As Double)
        Me.X = x
        Me.Y = y
    End Sub
End Structure
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Point" && *t == ChunkType::Struct),
        "Expected 'Point' struct, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_enum() {
    let content = r#"
Public Enum Status
    Active = 1
    Inactive = 2
    Pending = 3
End Enum
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let en = chunks.iter().find(|c| c.name == "Status");
    assert!(en.is_some(), "Expected 'Status' enum");
    assert_eq!(en.unwrap().chunk_type, ChunkType::Enum);
}

#[test]
fn parse_vbnet_property() {
    let content = r#"
Public Class Employee
    Private _name As String

    Public Property Name As String
        Get
            Return _name
        End Get
        Set(value As String)
            _name = value
        End Set
    End Property
End Class
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "Name" && *t == ChunkType::Property),
        "Expected 'Name' property, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_delegate_event() {
    let content = r#"
Public Class EventSource
    Public Delegate Sub NotifyHandler(message As String)
    Public Event OnNotify As NotifyHandler
End Class
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .map(|c| (c.name.as_str(), c.chunk_type))
        .collect();

    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "NotifyHandler" && *t == ChunkType::Delegate),
        "Expected 'NotifyHandler' delegate, got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|(n, t)| *n == "OnNotify" && *t == ChunkType::Event),
        "Expected 'OnNotify' event, got: {:?}",
        names
    );
}

#[test]
fn parse_vbnet_call_graph() {
    // NOTE: VB.NET grammar requires full class/module context, so use
    // parse_file_calls (production path) rather than extract_calls_from_chunk.
    let content = r#"
Module Program
    Sub Main()
        Dim calc As New Calculator()
        Dim result As Integer = calc.Add(1, 2)
        Console.WriteLine(result)
    End Sub
End Module
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let function_calls = parser.parse_file_calls(file.path()).unwrap();
    let main_fc = function_calls.iter().find(|fc| fc.name == "Main");
    assert!(main_fc.is_some(), "Expected 'Main' in call graph");
    let main_fc = main_fc.unwrap();
    let callee_names: Vec<_> = main_fc
        .calls
        .iter()
        .map(|c| c.callee_name.as_str())
        .collect();
    assert!(
        callee_names.contains(&"Add"),
        "Expected 'Add' call, got: {:?}",
        callee_names
    );
    assert!(
        callee_names.contains(&"WriteLine"),
        "Expected 'WriteLine' call, got: {:?}",
        callee_names
    );
}

#[test]
fn parse_vbnet_type_refs() {
    let content = r#"
Public Class DataProcessor
    Public Function Process(input As List(Of String), count As Integer) As Boolean
        Return True
    End Function
End Class
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "Process");
    assert!(func.is_some(), "Expected 'Process' function");
}

#[test]
fn parse_vbnet_no_code() {
    // Empty file should produce no chunks
    let content = r#"
' This is just a comment
Option Strict On
Imports System
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    // Should only have imports-related chunks or nothing
    let code_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| {
            matches!(
                c.chunk_type,
                ChunkType::Class | ChunkType::Method | ChunkType::Function
            )
        })
        .collect();
    assert!(
        code_chunks.is_empty(),
        "Expected no code chunks, got: {:?}",
        code_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

#[test]
fn parse_vbnet_field_declaration() {
    let content = r#"
Public Class Config
    Private _timeout As Integer
    Public Shared MaxRetries As Integer = 3
End Class
"#;
    let file = write_temp_file(content, "vb");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"_timeout"),
        "Expected '_timeout' field, got: {:?}",
        names
    );
}

// -- vue ─────────────────────────────────────────────────────────────

#[test]
fn parse_vue_with_script() {
    let content = r#"<template>
  <div>
    <h1>Hello World</h1>
    <button @click="handleClick">Click me</button>
  </div>
</template>

<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function formatName(first, last) {
    return `${first} ${last}`;
}
</script>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // JS functions should be extracted via injection
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.iter().any(|c| c.name == "handleClick"),
        "Expected JS function 'handleClick', got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
    assert!(
        js_chunks.iter().any(|c| c.name == "formatName"),
        "Expected JS function 'formatName'"
    );
}

#[test]
fn parse_vue_with_typescript() {
    let content = r#"<script lang="ts">
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello, ${user.name}!`;
}
</script>

<template>
  <p>Content</p>
</template>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let ts_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::TypeScript)
        .collect();
    assert!(
        ts_chunks.iter().any(|c| c.name == "greet"),
        "Expected TS function 'greet' from <script lang=\"ts\">, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_vue_with_style() {
    let content = r#"<template>
  <div class="app">Hello</div>
</template>

<style>
.app {
    color: red;
    font-size: 16px;
}

.container {
    display: flex;
}
</style>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let css_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::Css)
        .collect();
    assert!(
        !css_chunks.is_empty(),
        "Expected CSS chunks from <style> block, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_vue_setup_script() {
    let content = r#"<script setup>
function increment() {
    count.value++;
}
</script>

<template>
  <button @click="increment">Count</button>
</template>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    // JS function should be extracted via injection from <script setup>
    // Note: the outer script_element chunk is replaced by injected JS chunks
    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.iter().any(|c| c.name == "increment"),
        "Expected JS function 'increment' from <script setup>, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.language))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parse_vue_heading_extraction() {
    let content = r#"<template>
  <h1>Welcome Page</h1>
  <nav id="main-nav">
    <a href="/">Home</a>
  </nav>
</template>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let sections: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Section)
        .collect();

    assert!(
        sections.iter().any(|c| c.name == "Welcome Page"),
        "Expected heading 'Welcome Page', got: {:?}",
        sections.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        sections.iter().any(|c| c.name == "nav#main-nav"),
        "Expected landmark 'nav#main-nav'"
    );
}

#[test]
fn parse_vue_no_script() {
    let content = r#"<template>
  <div>Pure template, no script</div>
</template>
"#;
    let file = write_temp_file(content, "vue");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let js_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.language == Language::JavaScript)
        .collect();
    assert!(
        js_chunks.is_empty(),
        "Expected no JS chunks without <script>"
    );
}

// -- xml ─────────────────────────────────────────────────────────────

#[test]
fn parse_xml_root_elements() {
    let content = r#"<?xml version="1.0"?>
<catalog>
  <book>
    <title>Rust Programming</title>
    <author>Steve</author>
  </book>
  <book>
    <title>The C Language</title>
    <author>K&amp;R</author>
  </book>
</catalog>
"#;
    let file = write_temp_file(content, "xml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    // catalog is root, book is depth 2 — both should appear
    assert!(
        names.contains(&"catalog"),
        "Expected 'catalog', got: {:?}",
        names
    );
    assert!(
        names.contains(&"book"),
        "Expected 'book' at depth 2, got: {:?}",
        names
    );
    // title/author are depth 3 — should be filtered
    assert!(
        !names.contains(&"title"),
        "Deep 'title' should be filtered, got: {:?}",
        names
    );
}

#[test]
fn parse_xml_element_type() {
    let content = r#"<root><item/></root>"#;
    let file = write_temp_file(content, "xml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let root = chunks.iter().find(|c| c.name == "root");
    assert!(root.is_some(), "Expected 'root' element");
    assert_eq!(root.unwrap().chunk_type, ChunkType::Struct);
}

#[test]
fn parse_xml_no_calls() {
    let content = r#"<root><child/></root>"#;
    let file = write_temp_file(content, "xml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "XML should have no calls");
    }
}

// -- yaml ────────────────────────────────────────────────────────────

#[test]
fn parse_yaml_top_level_keys() {
    let content = r#"name: my-service
version: 1.0.0
dependencies:
  - redis
  - postgres
"#;
    let file = write_temp_file(content, "yaml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"name"),
        "Expected 'name' key, got: {:?}",
        names
    );
    assert!(
        names.contains(&"version"),
        "Expected 'version' key, got: {:?}",
        names
    );
}

#[test]
fn parse_yaml_chunk_type() {
    let content = r#"server:
  host: localhost
  port: 8080
"#;
    let file = write_temp_file(content, "yaml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let server = chunks.iter().find(|c| c.name == "server");
    assert!(server.is_some(), "Expected 'server' chunk");
    assert_eq!(server.unwrap().chunk_type, ChunkType::ConfigKey);
}

#[test]
fn parse_yaml_no_calls() {
    let content = r#"database:
  host: localhost
  port: 5432
"#;
    let file = write_temp_file(content, "yaml");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        assert!(calls.is_empty(), "YAML should have no calls");
    }
}

// -- zig ─────────────────────────────────────────────────────────────

#[test]
fn parse_zig_function() {
    let content = r#"
const std = @import("std");

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}
"#;
    let file = write_temp_file(content, "zig");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "add").unwrap();
    assert_eq!(func.chunk_type, ChunkType::Function);
}

#[test]
fn parse_zig_struct() {
    let content = r#"
const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point, other: Point) f32 {
        const dx = self.x - other.x;
        const dy = self.y - other.y;
        return @sqrt(dx * dx + dy * dy);
    }
};
"#;
    let file = write_temp_file(content, "zig");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let s = chunks.iter().find(|c| c.name == "Point").unwrap();
    assert_eq!(s.chunk_type, ChunkType::Struct);
}

#[test]
fn parse_zig_enum() {
    let content = r#"
const Color = enum {
    red,
    green,
    blue,
};
"#;
    let file = write_temp_file(content, "zig");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let e = chunks.iter().find(|c| c.name == "Color").unwrap();
    assert_eq!(e.chunk_type, ChunkType::Enum);
}

#[test]
fn parse_zig_calls() {
    let content = r#"
const std = @import("std");

pub fn process(allocator: std.mem.Allocator) void {
const list = std.ArrayList(u8).init(allocator);
std.debug.print("processing\n", .{});
}
"#;
    let file = write_temp_file(content, "zig");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "process").unwrap();
    let calls = parser.extract_calls_from_chunk(func);
    let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
    assert!(
        names.contains(&"init") || names.contains(&"print"),
        "Expected member calls, got: {:?}",
        names
    );
}

// -- dart ────────────────────────────────────────────────────────────

#[test]
fn test_dart_parse_function() {
    let content = r#"
Map<String, dynamic> parseConfig(String path) {
  final file = File(path);
  return jsonDecode(file.readAsStringSync());
}
"#;
    let file = write_temp_file(content, "dart");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "parseConfig" && c.chunk_type == ChunkType::Function),
        "Expected parseConfig function, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_dart_parse_class() {
    let content = r#"
class User {
  final String name;
  final int age;

  User(this.name, this.age);

  String getDisplayName() {
    return '$name ($age)';
  }
}
"#;
    let file = write_temp_file(content, "dart");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "User" && c.chunk_type == ChunkType::Class),
        "Expected User class, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_dart_parse_enum() {
    let content = r#"
enum Status {
  active,
  inactive,
  pending,
}
"#;
    let file = write_temp_file(content, "dart");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "Status" && c.chunk_type == ChunkType::Enum),
        "Expected Status enum, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_dart_parse_method() {
    let content = r#"
class User {
  final String name;

  String getDisplayName() {
    return name.toUpperCase();
  }

  bool isValid() => name.isNotEmpty;
}
"#;
    let file = write_temp_file(content, "dart");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "User" && c.chunk_type == ChunkType::Class),
        "Expected User class, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
    assert!(
        chunks
            .iter()
            .any(|c| c.name == "getDisplayName" && c.chunk_type == ChunkType::Method),
        "Expected getDisplayName method, got: {:?}",
        chunks
            .iter()
            .map(|c| (&c.name, &c.chunk_type))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_dart_doc_comment() {
    let content = r#"
/// Parse a configuration file from disk
Map<String, dynamic> parseConfig(String path) {
  return {};
}
"#;
    let file = write_temp_file(content, "dart");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "parseConfig").unwrap();
    assert!(
        func.doc.as_ref().map_or(false, |d| d.contains("Parse")),
        "Expected doc comment, got: {:?}",
        func.doc
    );
}

// -- Phase 2 chunk type tests ────────────────────────────────────────

#[test]
fn parse_javascript_describe_it_test_blocks() {
    let content = r#"
describe("UserService", () => {
    it("should create a user", () => {
        expect(true).toBe(true);
    });

    test("handles errors", () => {
        expect(false).toBe(false);
    });
});

function helperFunction() {
    return 42;
}
"#;
    let file = write_temp_file(content, "js");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let describe = chunks.iter().find(|c| c.name == "UserService");
    assert!(
        describe.is_some(),
        "Should find describe block 'UserService'"
    );
    assert_eq!(describe.unwrap().chunk_type, ChunkType::Test);
    let it_block = chunks.iter().find(|c| c.name == "should create a user");
    assert!(it_block.is_some(), "Should find it block");
    assert_eq!(it_block.unwrap().chunk_type, ChunkType::Test);
    let test_block = chunks.iter().find(|c| c.name == "handles errors");
    assert!(test_block.is_some(), "Should find test block");
    assert_eq!(test_block.unwrap().chunk_type, ChunkType::Test);
    let helper = chunks.iter().find(|c| c.name == "helperFunction").unwrap();
    assert_eq!(helper.chunk_type, ChunkType::Function);
}

#[test]
fn parse_typescript_describe_it_test_blocks() {
    let content = r#"
describe("Calculator", () => {
    it("adds numbers", () => {
        expect(add(1, 2)).toBe(3);
    });
});
"#;
    let file = write_temp_file(content, "ts");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let describe = chunks.iter().find(|c| c.name == "Calculator");
    assert!(
        describe.is_some(),
        "Should find describe block 'Calculator'"
    );
    assert_eq!(describe.unwrap().chunk_type, ChunkType::Test);
}

#[test]
fn parse_python_flask_endpoint() {
    let content = r#"
from flask import Flask
app = Flask(__name__)

@app.route("/users")
def list_users():
    return []

@app.get("/users/<int:id>")
def get_user(id):
    return {"id": id}

@app.post("/users")
def create_user():
    return {"created": True}

def helper():
    pass
"#;
    let file = write_temp_file(content, "py");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let list_users = chunks.iter().find(|c| c.name == "list_users").unwrap();
    assert_eq!(list_users.chunk_type, ChunkType::Endpoint);
    let get_user = chunks.iter().find(|c| c.name == "get_user").unwrap();
    assert_eq!(get_user.chunk_type, ChunkType::Endpoint);
    let create_user = chunks.iter().find(|c| c.name == "create_user").unwrap();
    assert_eq!(create_user.chunk_type, ChunkType::Endpoint);
    let helper = chunks.iter().find(|c| c.name == "helper").unwrap();
    assert_eq!(helper.chunk_type, ChunkType::Function);
}

#[test]
fn parse_java_test_annotation() {
    let content = r#"
import org.junit.jupiter.api.Test;

public class UserTest {
    @Test
    void shouldCreateUser() {
        assert(true);
    }

    void helperMethod() {
        // not a test
    }
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let test_fn = chunks.iter().find(|c| c.name == "shouldCreateUser");
    assert!(test_fn.is_some(), "Should find @Test method");
    assert_eq!(test_fn.unwrap().chunk_type, ChunkType::Test);
    let helper = chunks.iter().find(|c| c.name == "helperMethod");
    assert!(helper.is_some(), "Should find helper method");
    assert_eq!(helper.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn parse_java_spring_endpoint() {
    let content = r#"
import org.springframework.web.bind.annotation.*;

@RestController
public class UserController {
    @GetMapping("/users")
    public List<User> getUsers() {
        return userService.findAll();
    }

    @PostMapping("/users")
    public User createUser(@RequestBody User user) {
        return userService.save(user);
    }

    private void validate(User user) {
        // not an endpoint
    }
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let get = chunks.iter().find(|c| c.name == "getUsers");
    assert!(get.is_some(), "Should find @GetMapping endpoint");
    assert_eq!(get.unwrap().chunk_type, ChunkType::Endpoint);
    let post = chunks.iter().find(|c| c.name == "createUser");
    assert!(post.is_some(), "Should find @PostMapping endpoint");
    assert_eq!(post.unwrap().chunk_type, ChunkType::Endpoint);
    let validate = chunks.iter().find(|c| c.name == "validate");
    assert!(validate.is_some(), "Should find private method");
    assert_eq!(validate.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn parse_csharp_test_attributes() {
    let content = r#"
using NUnit.Framework;

[TestFixture]
public class CalculatorTests {
    [Test]
    public void Add_ReturnsSum() {
        Assert.AreEqual(4, 2 + 2);
    }

    [Fact]
    public void Subtract_ReturnsDifference() {
        Assert.Equal(0, 2 - 2);
    }

    private void Setup() {
        // not a test
    }
}
"#;
    let file = write_temp_file(content, "cs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let nunit = chunks.iter().find(|c| c.name == "Add_ReturnsSum");
    assert!(nunit.is_some(), "Should find [Test] method");
    assert_eq!(nunit.unwrap().chunk_type, ChunkType::Test);
    let xunit = chunks
        .iter()
        .find(|c| c.name == "Subtract_ReturnsDifference");
    assert!(xunit.is_some(), "Should find [Fact] method");
    assert_eq!(xunit.unwrap().chunk_type, ChunkType::Test);
    let setup = chunks.iter().find(|c| c.name == "Setup");
    assert!(setup.is_some(), "Should find Setup method");
    assert_eq!(setup.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn parse_csharp_aspnet_endpoint() {
    let content = r#"
using Microsoft.AspNetCore.Mvc;

[ApiController]
[Route("api/[controller]")]
public class UsersController : ControllerBase {
    [HttpGet]
    public IActionResult GetAll() {
        return Ok();
    }

    [HttpPost]
    public IActionResult Create([FromBody] User user) {
        return Created();
    }

    private bool Validate(User user) {
        return true;
    }
}
"#;
    let file = write_temp_file(content, "cs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let get = chunks.iter().find(|c| c.name == "GetAll");
    assert!(get.is_some(), "Should find [HttpGet] endpoint");
    assert_eq!(get.unwrap().chunk_type, ChunkType::Endpoint);
    let post = chunks.iter().find(|c| c.name == "Create");
    assert!(post.is_some(), "Should find [HttpPost] endpoint");
    assert_eq!(post.unwrap().chunk_type, ChunkType::Endpoint);
    let validate = chunks.iter().find(|c| c.name == "Validate");
    assert!(validate.is_some(), "Should find private method");
    assert_eq!(validate.unwrap().chunk_type, ChunkType::Method);
}

// -- Audit: post_process reclassification preservation tests ──────────
// Verify that non-reclassified chunks stay as their raw capture type.
// Catches bugs where post_process silently fixes a wrong query capture.

#[test]
fn audit_java_static_final_constant() {
    let content = r#"
public class Config {
    public static final int MAX_RETRIES = 3;
    public static final String API_URL = "https://example.com";
    public int normalField = 42;
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
    assert!(max.is_some(), "Should find static final constant");
    assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
}

#[test]
fn audit_java_regular_method_stays_method() {
    let content = r#"
public class Service {
    public void process() {
        System.out.println("working");
    }
}
"#;
    let file = write_temp_file(content, "java");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "process");
    assert!(method.is_some(), "Should find regular method");
    assert_eq!(method.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn audit_typescript_let_var_is_variable() {
    let content = r#"
let counter: number = 0;
var legacy: string = "old";
const MAX: number = 100;
"#;
    let file = write_temp_file(content, "ts");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let let_var = chunks.iter().find(|c| c.name == "counter");
    assert!(let_var.is_some(), "Should find let declaration as Variable");
    assert_eq!(let_var.unwrap().chunk_type, ChunkType::Variable);
    let var_var = chunks.iter().find(|c| c.name == "legacy");
    assert!(var_var.is_some(), "Should find var declaration as Variable");
    assert_eq!(var_var.unwrap().chunk_type, ChunkType::Variable);
    let const_var = chunks.iter().find(|c| c.name == "MAX");
    assert!(const_var.is_some(), "Should find const as Constant");
    assert_eq!(const_var.unwrap().chunk_type, ChunkType::Constant);
}

#[test]
fn audit_javascript_function_stays_function() {
    let content = r#"
function processData(input) {
    return input.map(x => x * 2);
}
"#;
    let file = write_temp_file(content, "js");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let func = chunks.iter().find(|c| c.name == "processData");
    assert!(func.is_some(), "Should find regular function");
    assert_eq!(func.unwrap().chunk_type, ChunkType::Function);
}

#[test]
fn audit_csharp_method_stays_method() {
    let content = r#"
public class Service {
    public void Process() {
        Console.WriteLine("working");
    }
}
"#;
    let file = write_temp_file(content, "cs");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    let method = chunks.iter().find(|c| c.name == "Process");
    assert!(method.is_some(), "Should find regular method");
    assert_eq!(method.unwrap().chunk_type, ChunkType::Method);
}

// -- Elm ─────────────────────────────────────────────────────────────

#[test]
fn parse_elm_function_and_types() {
    let content = r#"
module Main exposing (main, update, view)

type Msg
    = Increment
    | Decrement

type alias Model =
    { count : Int
    }

update : Msg -> Model -> Model
update msg model =
    case msg of
        Increment ->
            { model | count = model.count + 1 }

        Decrement ->
            { model | count = model.count - 1 }

view : Model -> Html Msg
view model =
    text (String.fromInt model.count)

main =
    text "Hello"
"#;
    let file = write_temp_file(content, "elm");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();

    let update_fn = chunks.iter().find(|c| c.name == "update");
    assert!(update_fn.is_some(), "Should find 'update' function");
    assert_eq!(update_fn.unwrap().chunk_type, ChunkType::Function);

    let view_fn = chunks.iter().find(|c| c.name == "view");
    assert!(view_fn.is_some(), "Should find 'view' function");
    assert_eq!(view_fn.unwrap().chunk_type, ChunkType::Function);

    let msg_type = chunks.iter().find(|c| c.name == "Msg");
    assert!(msg_type.is_some(), "Should find 'Msg' type");
    assert_eq!(msg_type.unwrap().chunk_type, ChunkType::Enum);

    let model_alias = chunks.iter().find(|c| c.name == "Model");
    assert!(model_alias.is_some(), "Should find 'Model' type alias");
    assert_eq!(model_alias.unwrap().chunk_type, ChunkType::TypeAlias);
}
