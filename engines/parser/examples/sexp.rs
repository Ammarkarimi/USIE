//! Debug helper for writing extraction adapters: prints the tree-sitter
//! syntax tree of a file with field names.
//!
//! `cargo run -p usie-parser --example sexp -- <language> <file>`
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lang: tree_sitter::Language = match args[1].as_str() {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        other => panic!("unknown language {other}"),
    };
    let src = std::fs::read(&args[2]).expect("read");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("grammar");
    let tree = parser.parse(&src, None).expect("parse");
    print(tree.root_node(), &src, 0, None);
}

fn print(node: tree_sitter::Node, src: &[u8], depth: usize, field: Option<&str>) {
    if node.is_named() {
        let text = if node.child_count() == 0 {
            format!(" {:?}", node.utf8_text(src).unwrap_or(""))
        } else {
            String::new()
        };
        let field = field.map(|f| format!("{f}: ")).unwrap_or_default();
        println!("{}{field}{}{text}", "  ".repeat(depth), node.kind());
    }
    let mut cursor = node.walk();
    for (i, child) in node.children(&mut cursor).enumerate() {
        print(child, src, depth + 1, node.field_name_for_child(i as u32));
    }
}
