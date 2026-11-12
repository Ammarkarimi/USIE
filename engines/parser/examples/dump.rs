//! Debug helper: `cargo run --example dump -- <language> <file>` prints the IR as JSON.
use std::io::Read;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lang = usie_ir::Language::parse(&args[1]).expect("language");
    let mut src = Vec::new();
    if args.len() > 2 {
        src = std::fs::read(&args[2]).expect("read");
    } else {
        std::io::stdin().read_to_end(&mut src).expect("stdin");
    }
    let ir = usie_parser::parse(lang, &src, &usie_parser::ParseOptions::default()).expect("parse");
    println!("{}", serde_json::to_string_pretty(&ir).unwrap());
}
