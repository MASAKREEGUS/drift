use drift::ast::*;
use drift::diag::apply_edits;
use drift::parser::parse;

fn codes(src: &str) -> Vec<&'static str> {
    parse(src).diags.iter().map(|d| d.code).collect()
}

/// Applying every fix must produce code without that diagnostic.
fn assert_fix_clears(src: &str, code: &str) {
    let out = parse(src);
    let d = out.diags.iter().find(|d| d.code == code).unwrap_or_else(|| panic!("no {} in {:?}", code, out.diags));
    assert!(!d.fix.is_empty(), "{} has no fix", code);
    let fixed = apply_edits(src, &d.fix);
    let after = codes(&fixed);
    assert!(after.is_empty(), "after fix:\n{}\nstill: {:?}", fixed, after);
}

#[test]
fn corpus_parses_without_diagnostics() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("dr") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let out = parse(&src);
        assert!(out.diags.is_empty(), "{}: {:#?}", path.display(), out.diags);
        assert!(!out.module.items.is_empty(), "{} parsed to nothing", path.display());
        n += 1;
    }
    assert!(n >= 9, "expected the example corpus, found {} files", n);
}

#[test]
fn missing_return_type_has_fix() {
    assert_eq!(codes("fn f(x: Int) { x }"), vec!["P0022"]);
    assert_fix_clears("fn f(x: Int) { x }", "P0022");
}

#[test]
fn angle_generics_point_to_brackets() {
    assert_fix_clears("fn f(xs: List<Int>) -> Int { 0 }", "P0031");
}

#[test]
fn c_style_logic_operators_have_fixes() {
    assert_fix_clears("fn f(a: Bool, b: Bool) -> Bool { a && b }", "L0001");
    assert_fix_clears("fn f(a: Bool) -> Bool { !a }", "L0001");
}

#[test]
fn chained_comparison_rejected() {
    assert!(codes("fn f() -> Bool { 1 < 2 < 3 }").contains(&"P0060"));
}

#[test]
fn no_globals() {
    assert_eq!(codes("let x = 1"), vec!["P0010"]);
}

#[test]
fn reports_every_error_in_one_run() {
    let src = "fn a(x: Int) { x }\nfn b(y: List<Int>) -> Int { 0 }\nfn c() -> Int { 1 < 2 < 3 }\n";
    let c = codes(src);
    assert!(c.contains(&"P0022") && c.contains(&"P0031") && c.contains(&"P0060"), "{:?}", c);
}

#[test]
fn interpolation_is_parsed_as_expressions() {
    let out = parse("fn f(x: Int) -> Str { \"a {x + 1} b {x}\" }");
    assert!(out.diags.is_empty(), "{:?}", out.diags);
    let ItemKind::Fn(f) = &out.module.items[0].kind else { panic!() };
    let StmtKind::Expr(e) = &f.body.stmts[0].kind else { panic!() };
    let ExprKind::Str(parts) = &e.kind else { panic!("{:?}", e) };
    assert_eq!(parts.len(), 4);
    assert!(matches!(&parts[1], StrPart::Expr(Expr { kind: ExprKind::Binary { op: BinOp::Add, .. }, .. })));
}

#[test]
fn leading_dot_continues_the_line() {
    let src = "fn f(xs: List[Int]) -> Int {\n  xs\n    .map(|x| x * 2)\n    .sum()\n}\n";
    let out = parse(src);
    assert!(out.diags.is_empty(), "{:?}", out.diags);
    let ItemKind::Fn(f) = &out.module.items[0].kind else { panic!() };
    assert_eq!(f.body.stmts.len(), 1);
}

#[test]
fn struct_literal_not_parsed_in_if_head() {
    let src = "fn f(p: Point) -> Bool {\n  if p == (Point { x: 0 }) { true } else { false }\n}\n";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
}

#[test]
fn precedence() {
    let out = parse("fn f(a: Int, b: Int) -> Bool { not a + 1 * b > 2 and a < b or a == 0 }");
    assert!(out.diags.is_empty());
    let ItemKind::Fn(f) = &out.module.items[0].kind else { panic!() };
    let StmtKind::Expr(e) = &f.body.stmts[0].kind else { panic!() };
    // top level must be `or`
    assert!(matches!(e.kind, ExprKind::Binary { op: BinOp::Or, .. }));
}

fn assert_all_fixes_clear(src: &str, expect: usize) {
    let out = parse(src);
    assert_eq!(out.diags.len(), expect, "{:#?}", out.diags);
    let edits: Vec<_> = out.diags.iter().flat_map(|d| d.fix.clone()).collect();
    let fixed = apply_edits(src, &edits);
    assert!(codes(&fixed).is_empty(), "after fixes:\n{}\n{:?}", fixed, codes(&fixed));
}

#[test]
fn lexer_recovers_without_cascade() {
    assert_all_fixes_clear("fn ok(a: Bool, b: Bool) -> Bool { a && !b }", 2);
    assert_all_fixes_clear("fn ok(a: Bool, b: Bool) -> Bool { a || b }", 1);
}

#[test]
fn missing_interpolation_brace_is_one_diagnostic() {
    assert_all_fixes_clear("fn greet(name: Str) -> Str { \"hola {name\" }", 1);
}

#[test]
fn chained_comparison_fix() {
    assert_all_fixes_clear("fn r(x: Int) -> Bool { 0 < x < 10 }", 1);
}

#[test]
fn diagnostics_sorted_by_position() {
    let out = parse("fn a(x: List<Int>) { x }\nfn b() -> Bool { 1 && 2 }");
    let los: Vec<u32> = out.diags.iter().map(|d| d.span.lo).collect();
    let mut sorted = los.clone();
    sorted.sort();
    assert_eq!(los, sorted);
}

#[test]
fn empty_closure_still_parses() {
    assert!(codes("fn f() -> Int { let g = || 1\n  g() }").is_empty());
}
