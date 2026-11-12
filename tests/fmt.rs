use drift::fmt::format_source;
use drift::parser::parse;

/// AST debug dump with all spans removed, for layout-independent comparison.
fn shape(src: &str) -> String {
    let out = parse(src);
    assert!(out.diags.is_empty(), "{:#?}", out.diags);
    let dbg = format!("{:?}", out.module);
    let mut res = String::with_capacity(dbg.len());
    let mut rest = dbg.as_str();
    while let Some(i) = rest.find("Span { lo: ") {
        res.push_str(&rest[..i]);
        let j = rest[i..].find('}').unwrap();
        res.push('S');
        rest = &rest[i + j + 1..];
    }
    res.push_str(rest);
    res
}

fn fmt(src: &str) -> String {
    match format_source(src) {
        Ok(s) => s,
        Err(_) => panic!("does not parse:\n{}", src),
    }
}

fn comments(src: &str) -> Vec<String> {
    parse(src).comments.iter().map(|c| c.text.trim_end().to_string()).collect()
}

fn check_invariants(name: &str, src: &str) {
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(once, twice, "{}: not idempotent", name);
    assert_eq!(shape(src), shape(&once), "{}: AST changed\n{}", name, once);
    assert_eq!(comments(src), comments(&once), "{}: comments changed", name);
    for (i, line) in once.lines().enumerate() {
        assert!(line.chars().count() <= drift::fmt::WIDTH || line.trim_start().starts_with("//") || line.contains('"'), "{}:{} too long: {}", name, i + 1, line);
        assert!(!line.ends_with(' '), "{}:{} trailing space", name, i + 1);
    }
}

#[test]
fn corpus_invariants() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("dr") {
            check_invariants(&path.display().to_string(), &std::fs::read_to_string(&path).unwrap());
        }
    }
}

#[test]
fn parentheses_restored_by_precedence() {
    let out = fmt("fn f(a: Int, b: Int) -> Int { (a - b) * (a + 1) - (b - a) }");
    assert!(out.contains("(a - b) * (a + 1) - (b - a)"), "{}", out);
    let out = fmt("fn f(a: Int, b: Int) -> Int { a - (b - a) }");
    assert!(out.contains("a - (b - a)"), "{}", out);
    let out = fmt("fn f(a: Bool, b: Bool) -> Bool { not (a and b) }");
    assert!(out.contains("not (a and b)"), "{}", out);
}

#[test]
fn struct_literal_in_condition_keeps_parens() {
    let src = "fn f(p: Point) -> Bool {\n  if p == (Point { x: 0 }) { true } else { false }\n}\n";
    check_invariants("cond", src);
    assert!(fmt(src).contains("if p == (Point { x: 0 }) {"));
}

#[test]
fn canonical_layout() {
    let src = "pub   fn  add( a :Int,b: Int )->Int{a+b}\nstruct P{x:Float\ny:Float}\nenum E { A, B(Int), C { x: Int } }";
    let out = fmt(src);
    assert_eq!(
        out,
        "pub fn add(a: Int, b: Int) -> Int {\n  a + b\n}\n\nstruct P { x: Float, y: Float }\n\nenum E {\n  A\n  B(Int)\n  C { x: Int }\n}\n"
    );
}

#[test]
fn long_calls_break_and_chains_break_before_dots() {
    let src = "fn f(xs: List[Int]) -> Int { xs.filter(|x| x > 1000000000000).map(|x| x * 2000000000000).filter(|x| x % 3 == 0).map(|x| x + 77777777).sum() }";
    let out = fmt(src);
    assert!(out.contains("\n    .map("), "{}", out);
    check_invariants("chain", src);
}

#[test]
fn comments_and_blank_lines_survive() {
    let src = "// header\n\n/// doc\nfn f() -> Int {\n  let a = 1 // trailing\n\n\n  // lead\n  a\n  // end\n}\n// eof\n";
    let out = fmt(src);
    assert_eq!(out, "// header\n\n/// doc\nfn f() -> Int {\n  let a = 1 // trailing\n\n  // lead\n  a\n  // end\n}\n// eof\n");
    check_invariants("comments", src);
}

#[test]
fn strings_round_trip() {
    check_invariants("str", "fn f(x: Int) -> Str { \"a\\tb \\{x\\} {x + 1} \\\"q\\\" \\u{1F600}\" }");
    check_invariants("bytes", "fn f() -> Bytes { b\"\\x00ab\\n\" }");
    check_invariants("triple", "fn f() -> Str {\n  \"\"\"\n    line one\n      two\n    \"\"\"\n}\n");
}

#[test]
fn map_and_with_bodies_get_parens() {
    let src = "fn f(p: P) -> P { let g = |x| ({\"a\": x})\n  match p { _ => ({ p with x: 1 }) } }";
    check_invariants("bodies", src);
}

#[test]
fn long_signature_breaks_params_not_return_type() {
    let src = "fn read_record(file: fs.File, offset: Int) -> Result[Option[(Str, Bytes, Int)], StoreError] uses fs { todo }";
    let out = fmt(src);
    assert!(out.starts_with("fn read_record(\n  file: fs.File,\n  offset: Int,\n) -> Result[Option[(Str, Bytes, Int)], StoreError] uses fs {"), "{}", out);
}

#[test]
fn raw_triple_strings_round_trip() {
    let src = "fn page() -> Str {\n  r\"\"\"\n    <p class=\"x\">{a}</p>\n      <b>\\n</b>\n\n    end\n    \"\"\"\n}\n";
    let out = drift::fmt::format_source(src).ok().expect("formats");
    assert_eq!(drift::fmt::format_source(&out).ok().unwrap(), out, "not idempotent:\n{}", out);
    let prog = drift::parser::parse(&out);
    assert!(prog.diags.is_empty(), "{:?}", prog.diags);
    let found = format!("{:?}", prog.module.items[0]);
    assert!(found.contains(r#"<p class=\"x\">{a}</p>\n  <b>\\n</b>\n\nend"#), "{}\n{}", out, found);
    assert_eq!(drift::fmt::format_source("fn f() -> Str {\n  r\"\"\"say \"hi\" now\"\"\"\n}\n").ok().unwrap(), "fn f() -> Str {\n  r\"\"\"say \"hi\" now\"\"\"\n}\n");
}
