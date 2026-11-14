//! Property tests: the parser must uphold its structural invariants on
//! arbitrary input, including input that is not valid source or not UTF-8.

use proptest::prelude::*;
use usie_ir::{FileIr, Language};
use usie_parser::{parse, ParseOptions};

const LANGUAGES: [Language; 4] = [
    Language::Python,
    Language::Javascript,
    Language::Typescript,
    Language::Tsx,
];

fn check_invariants(ir: &FileIr, len: usize) {
    let in_bounds = |s: &usie_ir::Span| {
        assert!(
            s.start_byte <= s.end_byte && s.end_byte as usize <= len,
            "span out of bounds: {s:?}"
        );
        assert!(s.start_line >= 1 && s.start_line <= s.end_line);
    };
    for (i, sym) in ir.symbols.iter().enumerate() {
        assert_eq!(sym.id as usize, i, "ids are dense and ordered");
        if let Some(p) = sym.parent {
            assert!(p < sym.id, "parents precede children");
            let parent = &ir.symbols[p as usize];
            assert_eq!(
                sym.qualified_name,
                format!("{}.{}", parent.qualified_name, sym.name)
            );
        } else {
            assert_eq!(sym.qualified_name, sym.name);
        }
        in_bounds(&sym.span);
        in_bounds(&sym.name_span);
    }
    let count = ir.symbols.len() as u32;
    for r in &ir.references {
        assert!(r.scope.is_none_or(|s| s < count));
        in_bounds(&r.span);
    }
    for i in &ir.imports {
        assert!(i.scope.is_none_or(|s| s < count));
        in_bounds(&i.span);
    }
    for e in &ir.exports {
        in_bounds(&e.span);
    }
    for d in &ir.diagnostics {
        in_bounds(&d.span);
    }
    assert!(ir.stats.diagnostics_total >= ir.diagnostics.len() as u64);
    // The IR always survives a JSON round trip.
    let json = serde_json::to_string(ir).unwrap();
    let back: FileIr = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, ir);
}

/// Source-like text: fragments that exercise the adapters, glued at random.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("def f(a, *b, c=1):\n    ".to_string()),
        Just("class C(B, metaclass=M):\n    ".to_string()),
        Just("import a.b as c\n".to_string()),
        Just("from ..x import y, z as w\n".to_string()),
        Just("__all__ = ['a', b]\n".to_string()),
        Just("export default ".to_string()),
        Just("export const x = ".to_string()),
        Just("export { a as b } from './m';".to_string()),
        Just("import d, { e as f } from 'm';".to_string()),
        Just("class K extends A.B implements I { static m = () => ".to_string()),
        Just("function g({ a, b: [c] }, ...r) { ".to_string()),
        Just("interface I extends J { m(): void }".to_string()),
        Just("namespace N { export function h() {} }".to_string()),
        Just("const { p, q: r } = require('z');".to_string()),
        Just("module.exports = { a, b: c };".to_string()),
        Just("<Tag.Sub attr={v}>".to_string()),
        Just("@dec(1)\n".to_string()),
        Just("a.b.c(d)(e);".to_string()),
        Just("new X.Y();".to_string()),
        Just("super().m()\n".to_string()),
        Just("=> {".to_string()),
        Just("}".to_string()),
        Just(")".to_string()),
        Just("(".to_string()),
        Just("\n".to_string()),
        Just("    ".to_string()),
        "[a-zA-Z_#@.:;,=(){}<>\\[\\]'\"` \n*/-]{0,12}",
        "\\PC{0,6}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_break_invariants(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        for language in LANGUAGES {
            let ir = parse(language, &bytes, &ParseOptions::default()).unwrap();
            check_invariants(&ir, bytes.len());
        }
    }

    #[test]
    fn source_like_text_never_breaks_invariants(parts in proptest::collection::vec(fragment(), 0..40)) {
        let src = parts.concat();
        for language in LANGUAGES {
            let ir = parse(language, src.as_bytes(), &ParseOptions::default()).unwrap();
            check_invariants(&ir, src.len());
        }
    }
}
