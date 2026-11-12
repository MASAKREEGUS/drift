use drift::check::SourceFile;
use drift::diag::{Diag, Severity};
use drift::driver::{analyze, analyze_one, fix_one};

fn semantic(src: &str) -> Vec<Diag> {
    // layout is tested elsewhere
    analyze_one(src).into_iter().filter(|d| d.code != "F0001").collect()
}

fn codes(src: &str) -> Vec<&'static str> {
    semantic(src).iter().map(|d| d.code).collect()
}

fn errors(src: &str) -> Vec<&'static str> {
    semantic(src).iter().filter(|d| d.is_error()).map(|d| d.code).collect()
}

/// `src` reports `code`; after `--fix` there are no errors left (holes are allowed).
fn fixes(src: &str, code: &str) -> String {
    let c = codes(src);
    assert!(c.contains(&code), "expected {} in {:?}\n{}", code, semantic(src), src);
    let d = semantic(src).into_iter().find(|d| d.code == code).unwrap();
    assert!(!d.fix.is_empty(), "{} has no fix: {}", code, d.message);
    let fixed = fix_one(src);
    let left = errors(&fixed);
    assert!(left.is_empty(), "after fix:\n{}\nstill: {:?}", fixed, semantic(&fixed));
    fixed
}

fn clean(src: &str) {
    let d = semantic(src);
    assert!(d.iter().all(|d| !d.is_error() && d.severity != Severity::Warning), "{:#?}\n{}", d, src);
}

// ---------------------------------------------------------------- types

#[test]
fn int_where_str_expected_suggests_interpolation() {
    let out = fixes("fn f(n: Int) -> Str {\n  n\n}\n", "E0201");
    assert!(out.contains("\"{n}\""), "{}", out);
}

#[test]
fn plain_value_where_result_expected_wraps_ok() {
    let out = fixes("fn f(n: Int) -> Result[Int, Error] {\n  n + 1\n}\n", "E0201");
    assert!(out.contains("Ok(n + 1)"), "{}", out);
}

#[test]
fn int_literal_in_float_context() {
    let out = fixes("fn f(x: Float) -> Float {\n  x * 2\n}\n", "E0201");
    assert!(out.contains("x * 2.0"), "{}", out);
}

#[test]
fn int_variable_in_float_context() {
    let out = fixes("fn f(x: Float, n: Int) -> Float {\n  x * n\n}\n", "E0201");
    assert!(out.contains("n.to_float()"), "{}", out);
}

#[test]
fn arity_mismatch() {
    assert!(errors("fn g(a: Int, b: Int) -> Int {\n  a + b\n}\n\nfn f() -> Int {\n  g(1)\n}\n").contains(&"E0203"));
}

#[test]
fn block_without_value() {
    assert!(errors("fn f() -> Int {\n  let x = 1\n}\n").contains(&"E0206"));
}

#[test]
fn generic_functions_infer() {
    clean("fn first[T](xs: List[T]) -> Option[T] {\n  xs.get(0)\n}\n\nfn f() -> Int {\n  first([1, 2]).unwrap_or(0)\n}\n");
}

#[test]
fn closures_get_parameter_types_from_context() {
    clean("fn f(xs: List[Int]) -> Int {\n  xs.map(|x| x * 2).filter(|x| x > 3).sum()\n}\n");
}

#[test]
fn cannot_infer_empty_list() {
    assert!(errors("fn f() -> Int {\n  let xs = []\n  0\n}\n").contains(&"E0802"));
}

// ---------------------------------------------------------------- names

#[test]
fn typo_in_variable_is_fixed() {
    fixes("fn f(count: Int) -> Int {\n  cont + 1\n}\n", "E0100");
}

#[test]
fn transposed_letters_are_fixed() {
    fixes("fn f(count: Int) -> Int {\n  coutn + 1\n}\n", "E0100");
}

#[test]
fn one_unknown_name_reports_once() {
    let src = "fn f(lines: List[Str]) -> Int {\n  let xs = lines.map(|l| Ofer.parse(l)).filter(|o| o.is_some())\n  xs.len()\n}\n";
    assert_eq!(errors(src), vec!["E0100"], "{:#?}", semantic(src));
}

#[test]
fn far_typo_is_only_a_hint() {
    let d = semantic("fn f(total_price: Int) -> Int {\n  totl_prce + 1\n}\n");
    let e = d.iter().find(|d| d.code == "E0100").unwrap();
    assert!(e.fix.is_empty() && e.message.contains("did you mean `total_price`"), "{:?}", e);
}

#[test]
fn foreign_type_names_are_translated() {
    let out = fixes("fn f(s: String, xs: Vec[Int]) -> Int {\n  xs.len()\n}\n", "E0102");
    assert!(out.contains("s: Str") && out.contains("List[Int]"), "{}", out);
}

#[test]
fn std_types_are_qualified() {
    let out = fixes("fn f(e: HttpError) -> Int {\n  e.status\n}\n", "E0102");
    assert!(out.contains("e: http.HttpError"), "{}", out);
}

#[test]
fn std_functions_are_qualified() {
    let out = fixes("fn f() -> Result[Str, Error] uses fs {\n  Ok(read_str(\"a\")?)\n}\n", "E0100");
    assert!(out.contains("fs.read_str"), "{}", out);
}

#[test]
fn print_becomes_io_println() {
    let out = fixes("fn f() -> () uses io {\n  print(\"hi\")\n}\n", "E0100");
    assert!(out.contains("io.println(\"hi\")"), "{}", out);
}

#[test]
fn method_aliases_from_other_languages() {
    let out = fixes("fn f(xs: List[Int], s: Str) -> Bool {\n  xs.length() > 0 and s.toLowerCase() == \"a\"\n}\n", "E0312");
    assert!(out.contains("xs.len()") && out.contains("s.to_lower()"), "{}", out);
}

#[test]
fn free_call_of_method_becomes_method_call() {
    let out = fixes("fn f(xs: List[Int]) -> Int {\n  len(xs)\n}\n", "E0105");
    assert!(out.contains("xs.len()"), "{}", out);
}

#[test]
fn unwrap_becomes_question_mark() {
    let out = fixes("fn f(xs: List[Int]) -> Option[Int] {\n  let x = xs.get(0).unwrap()\n  Some(x + 1)\n}\n", "E0312");
    assert!(out.contains("xs.get(0)?"), "{}", out);
}

#[test]
fn unwrap_of_option_in_result_fn_becomes_a_hole() {
    let out = fixes("fn f(xs: List[Int]) -> Result[Int, Error] {\n  Ok(xs.get(0).unwrap())\n}\n", "E0312");
    assert!(out.contains("xs.get(0).ok_or(todo)?"), "{}", out);
}

#[test]
fn unknown_field_typo() {
    fixes("struct P { name: Str }\n\nfn f(p: P) -> Str {\n  p.nme\n}\n", "E0311");
}

#[test]
fn method_used_as_field_gets_parens() {
    let out = fixes("fn f(xs: List[Int]) -> Int {\n  xs.len\n}\n", "E0313");
    assert!(out.contains("xs.len()"), "{}", out);
}

#[test]
fn naming_conventions() {
    assert!(errors("fn fooBar() -> Int {\n  1\n}\n").contains(&"E0701"));
    assert!(errors("struct my_type { x: Int }\n").contains(&"E0701"));
}

#[test]
fn no_shadowing() {
    assert!(errors("fn f(x: Int) -> Int {\n  let x = 2\n  x\n}\n").contains(&"E0601"));
}

#[test]
fn std_imports_are_redundant() {
    fixes("import std.fs\n\nfn f() -> Bool uses fs {\n  fs.exists(\"a\")\n}\n", "W0001");
}

// ---------------------------------------------------------------- effects

#[test]
fn missing_effect_is_added_to_signature() {
    let out = fixes("fn f() -> () {\n  io.println(\"a\")\n}\n", "E0301");
    assert!(out.contains("fn f() -> () uses io {"), "{}", out);
}

#[test]
fn missing_effect_appends_to_existing_uses() {
    let out = fixes("fn f() -> Bool uses fs {\n  io.println(\"a\")\n  fs.exists(\"x\")\n}\n", "E0301");
    assert!(out.contains("uses fs, io {"), "{}", out);
}

#[test]
fn effects_flow_through_closures() {
    let out = fixes("fn f(xs: List[Str]) -> Int {\n  let ys = xs.map(|x| io.println(x))\n  ys.len()\n}\n", "E0301");
    assert!(out.contains("uses io"), "{}", out);
}

#[test]
fn several_missing_effects_all_added() {
    let out = fixes("fn f() -> Bool {\n  io.println(\"a\")\n  fs.exists(\"x\")\n}\n", "E0301");
    assert!(out.contains("uses io, fs"), "{}", out);
}

#[test]
fn spawn_is_an_effect() {
    fixes("fn g() -> Int {\n  1\n}\n\nfn f() -> Int uses spawn {\n  let t = spawn g()\n  t.join()\n}\n\nfn h() -> Int {\n  f()\n}\n", "E0301");
}

#[test]
fn unknown_effect_typo() {
    fixes("fn f() -> () uses ioo {\n  io.println(\"a\")\n}\n", "E0120");
}

#[test]
fn examples_must_be_pure_and_boolean() {
    let c = errors("fn f(n: Int) -> Int {\n  n\n}\n\nexamples f {\n  f(1)\n  io.println(\"x\") == ()\n}\n");
    assert!(c.contains(&"E0201") && c.contains(&"E0301"), "{:?}", c);
}

// ---------------------------------------------------------------- errors and ?

#[test]
fn ignored_result_is_an_error() {
    let out = fixes("fn f() -> Result[(), Error] uses fs {\n  fs.remove(\"a\")\n  Ok(())\n}\n", "E0250");
    assert!(out.contains("fs.remove(\"a\")?"), "{}", out);
}

#[test]
fn question_mark_needs_a_conversion() {
    let src = "enum MyError {\n  Bad\n}\n\nfn f() -> Result[Str, MyError] uses fs {\n  Ok(fs.read_str(\"a\")?)\n}\n";
    assert!(errors(src).contains(&"E0402"));
    let ok = "enum MyError {\n  Io(fs.IoError)\n  Bad\n}\n\nfn f() -> Result[Str, MyError] uses fs {\n  Ok(fs.read_str(\"a\")?)\n}\n";
    clean(ok);
}

#[test]
fn question_mark_on_option_in_result_fn() {
    let out = fixes("fn f(xs: List[Int]) -> Result[Int, Error] {\n  Ok(xs.get(0)?)\n}\n", "E0403");
    assert!(out.contains(".ok_or(todo)?"), "{}", out);
}

#[test]
fn any_error_converts_into_error() {
    clean("fn f() -> Result[Str, Error] uses fs {\n  Ok(fs.read_str(\"a\")?)\n}\n");
}

// ---------------------------------------------------------------- structs and match

#[test]
fn missing_struct_fields_become_holes() {
    let out = fixes("struct P { x: Int, y: Int, z: Int }\n\nfn f() -> P {\n  P { x: 1 }\n}\n", "E0310");
    assert!(out.contains("y: todo") && out.contains("z: todo"), "{}", out);
}

#[test]
fn non_exhaustive_match_adds_arms() {
    let src = "enum Dir {\n  Up\n  Down\n  Left\n}\n\nfn f(d: Dir) -> Int {\n  match d {\n    Dir.Up => 1\n  }\n}\n";
    let out = fixes(src, "E0280");
    assert!(out.contains("Dir.Down => todo") && out.contains("Dir.Left => todo"), "{}", out);
}

#[test]
fn exhaustive_matches_are_clean() {
    clean("fn f(b: Bool, xs: List[Int], o: Option[Int]) -> Int {\n  let a = match b {\n    true => 1\n    false => 2\n  }\n  let c = match xs {\n    [] => 0\n    [x, ..rest] => x + rest.len()\n  }\n  match o {\n    Some(v) => v + a + c\n    None => 0\n  }\n}\n");
}

#[test]
fn literal_match_needs_wildcard() {
    assert!(errors("fn f(n: Int) -> Int {\n  match n {\n    0 => 1\n    1 => 2\n  }\n}\n").contains(&"E0280"));
}

#[test]
fn unreachable_arm_warns() {
    let c = codes("fn f(o: Option[Int]) -> Int {\n  match o {\n    _ => 0\n    Some(x) => x\n  }\n}\n");
    assert!(c.contains(&"W0101"), "{:?}", c);
}

#[test]
fn struct_variant_pattern_needs_all_fields_or_rest() {
    fixes("enum S {\n  C { r: Float, x: Float }\n}\n\nfn f(s: S) -> Float {\n  match s {\n    S.C { r } => r\n  }\n}\n", "E0270");
}

#[test]
fn if_without_else_as_value() {
    fixes("fn f(b: Bool) -> Int {\n  if b {\n    1\n  }\n}\n", "E0230");
}

// ---------------------------------------------------------------- mutability

#[test]
fn assigning_a_let_suggests_var() {
    let out = fixes("fn f() -> Int {\n  let x = 1\n  x = 2\n  x\n}\n", "E0501");
    assert!(out.contains("var x = 1"), "{}", out);
}

#[test]
fn push_on_let_list_suggests_var() {
    fixes("fn f() -> Int {\n  let xs = [1]\n  xs.push(2)\n  xs.len()\n}\n", "E0501");
}

#[test]
fn unchanged_var_suggests_let() {
    fixes("fn f() -> Int {\n  var x = 1\n  x\n}\n", "W0201");
}

#[test]
fn methods_changing_self_need_var_self() {
    let out = fixes("struct C { n: Int }\n\nfn bump(self: C) -> () {\n  self.n += 1\n}\n", "E0501");
    assert!(out.contains("fn bump(var self: C)"), "{}", out);
}

#[test]
fn closures_cannot_mutate_captures() {
    assert!(errors("fn f(xs: List[Int]) -> Int {\n  var total = 0\n  let ys = xs.map(|x| {\n    total += x\n    x\n  })\n  total + ys.len()\n}\n").contains(&"E0503"));
}

// ---------------------------------------------------------------- holes

#[test]
fn holes_report_type_and_scope() {
    let d = semantic("fn f(name: Str, n: Int) -> List[Str] {\n  todo\n}\n");
    let h = d.iter().find(|d| d.code == "H0001").expect("hole");
    assert_eq!(h.severity, Severity::Hole);
    assert_eq!(h.expected.as_deref(), Some("List[Str]"));
    assert!(h.scope.contains(&("name".into(), "Str".into())) && h.scope.contains(&("n".into(), "Int".into())), "{:?}", h.scope);
}

// ---------------------------------------------------------------- modules

#[test]
fn user_modules_import_and_privacy() {
    let lib = SourceFile {
        name: "shop/offers.dr".into(),
        src: "pub fn discount(p: Int) -> Int {\n  p / 10\n}\n\nfn secret() -> Int {\n  1\n}\n".into(),
        module: "shop.offers".into(),
        is_std: false,
    };
    let main = SourceFile {
        name: "main.dr".into(),
        src: "import shop.offers.{discount}\nimport shop.offers\n\nfn f() -> Int {\n  discount(100) + offers.secret()\n}\n".into(),
        module: "main".into(),
        is_std: false,
    };
    let out = analyze(&[lib, main]);
    let c: Vec<&str> = out[1].iter().filter(|d| d.code != "F0001").map(|d| d.code).collect();
    assert_eq!(c, vec!["E0903"], "{:#?}", out[1]);
}

#[test]
fn std_library_itself_is_consistent() {
    // every std module parses, and a program touching all of them checks cleanly
    clean(
        "fn f() -> Result[(), Error] uses io, fs, env, net, time, rand {\n  io.println(\"{env.args().len()} {time.now_ms()} {rand.int(0, 10)}\")\n  let r = http.get(\"x\")?.text()?\n  let d = html.parse(r)\n  let t = d.select_first(\"title\").map(|n| n.text()).unwrap_or(\"\")\n  fs.write_str(\"out\", json.encode(t) + t)?\n  let b = bytes.u32_le(1)\n  io.println(\"{b.len()}\")\n  Ok(())\n}\n",
    );
}
