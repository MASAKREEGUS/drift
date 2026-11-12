//! End-to-end: Drift source -> C -> gcc -> run. Needs gcc on the machine.
use std::process::Command;

fn drift(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_drift"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run drift");
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

#[test]
fn golden_programs() {
    for (name, code) in [("values", 0), ("closures", 0), ("control", 0), ("errors", 1)] {
        let (out, err, c) = drift(&["run", &format!("tests/programs/{}.dr", name)]);
        assert_eq!(out, read(&format!("tests/programs/{}.out", name)), "{}: stderr:\n{}", name, err);
        assert_eq!(c, code, "{}: {}", name, err);
    }
}

#[test]
fn error_from_main_is_reported() {
    let (_, err, c) = drift(&["run", "tests/programs/errors.dr"]);
    assert_eq!(c, 1);
    assert!(err.contains("error: No such file or directory (path: \"/no/such/file\")"), "{}", err);
}

#[test]
fn examples_run() {
    assert_eq!(drift(&["run", "examples/hello.dr"]).0, "hola desde Drift\n");
    let fb = drift(&["run", "examples/fizzbuzz.dr"]).0;
    assert_eq!(fb.lines().take(5).collect::<Vec<_>>(), vec!["1", "2", "Fizz", "4", "Buzz"]);
    assert_eq!(fb.lines().nth(14), Some("FizzBuzz"));
    let (sh, _, _) = drift(&["run", "examples/shapes.dr"]);
    assert_eq!(sh, "total area: 13.141592653589793\nbiggest: Shape.Rect { origin: Point { x: 1.0, y: 1.0 }, w: 4.0, h: 2.5 }\n");
}

#[test]
fn program_arguments_and_files() {
    let dir = std::env::temp_dir().join(format!("drift-wc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("t.txt");
    std::fs::write(&f, "b a b\nÑandú ñandú!\n").unwrap();
    let (out, err, c) = drift(&["run", "examples/word_count.dr", f.to_str().unwrap()]);
    assert_eq!(c, 0, "{}", err);
    assert_eq!(out, "2\tb\n2\tñandú\n1\ta\n");
    let (_, err, c) = drift(&["run", "examples/word_count.dr"]);
    assert_eq!(c, 1);
    assert!(err.contains("usage: word_count <file>"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn examples_and_tests_pass() {
    for f in ["fizzbuzz", "shapes", "offers", "word_count", "kv_store"] {
        let (out, err, c) = drift(&["test", &format!("examples/{}.dr", f)]);
        assert_eq!(c, 0, "{}:\n{}{}", f, out, err);
        assert!(out.contains(" 0 failed"), "{}", out);
    }
}

#[test]
fn multi_module_project() {
    let (out, err, c) = drift(&["run", "tests/programs/project"]);
    assert_eq!((out.as_str(), c), ("cafe: 20%\npan: 10%\n", 0), "{}", err);
    let (out, _, c) = drift(&["test", "tests/programs/project"]);
    assert_eq!(c, 0, "{}", out);
}

#[test]
fn panics_exit_101_with_location() {
    for (f, msg) in [
        ("overflow", "panic at tests/programs/fail/overflow.dr:2: integer overflow in `*`"),
        ("index", "panic at tests/programs/fail/index.dr:4: index 5 out of range for a list of length 3"),
        ("contract", "`ensures` of `percent` failed: result >= 0"),
    ] {
        let (_, err, c) = drift(&["run", &format!("tests/programs/fail/{}.dr", f)]);
        assert_eq!(c, 101, "{}: {}", f, err);
        assert!(err.contains(msg), "{}: {}", f, err);
    }
}

#[test]
fn failing_tests_are_reported_and_isolated() {
    let (out, _, c) = drift(&["test", "tests/programs/fail/failing_test.dr"]);
    assert_eq!(c, 1);
    assert!(out.contains("FAIL examples double"), "{}", out);
    assert!(out.contains("example failed at tests/programs/fail/failing_test.dr:11: double(3) == 7"), "{}", out);
    assert!(out.contains("reached a `todo` hole"), "{}", out);
    assert!(out.contains("left:  2") && out.contains("right: 3"), "{}", out);
    assert!(out.contains("ok   test \"passes\""), "{}", out);
    assert!(out.contains("1 passed, 3 failed"), "{}", out);
    let (json, _, _) = drift(&["test", "--json", "tests/programs/fail/failing_test.dr"]);
    assert_eq!(json.lines().count(), 4);
    assert!(json.lines().all(|l| l.starts_with("{\"test\":")), "{}", json);
}

#[test]
fn run_rejects_holes_and_errors() {
    let (out, err, c) = drift(&["run", "tests/programs/fail/failing_test.dr"]);
    assert_eq!(c, 1);
    assert!(out.contains("H0001") && err.contains("hole"), "{}{}", out, err);
    let (out, _, c) = drift(&["run", "examples/holes.dr"]);
    assert_eq!(c, 1, "{}", out);
}

#[test]
fn only_phase4_natives_are_missing() {
    let (_, prog) = drift::driver::analyze_program(&[]);
    let missing = drift::codegen::missing_natives(&prog.unwrap());
    let unexpected: Vec<&String> = missing.iter().filter(|n| !n.starts_with("d_n_http_") && !n.starts_with("d_n_html_") && !n.starts_with("d_n_json_")).collect();
    assert!(unexpected.is_empty(), "std functions without a native implementation: {:?}", unexpected);
}
