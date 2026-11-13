"""Grades eval solutions against the hidden tests.

    python3 eval/grade.py submit <task> <drift|python|rust> <workdir>   # at most 3 per task and language
    python3 eval/grade.py bench <run> [repeats]                          # times the final solutions' perf cases
    python3 eval/grade.py report <run>

The workdir holds main.dr, main.py or main.rs. Each submission is copied to eval/results/<run>/ with its result.
The run name comes from $EVAL_RUN (default "run1").
"""
import fcntl, json, os, pathlib, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import reference as R

EVAL = pathlib.Path(__file__).resolve().parent
DRIFT = EVAL.parent / "target/release/drift"
RUSTC = pathlib.Path.home() / ".cargo/bin/rustc"
SRC = {"drift": "main.dr", "python": "main.py", "rust": "main.rs"}
TIMEOUT = 180  # wall seconds; generous because the machine also serves production. `bench` measures CPU time.
MAX_SUBMISSIONS = 3


def build(lang, src, out_dir):
    """Returns (command, error text)."""
    exe = out_dir / "prog"
    if lang == "python":
        return [sys.executable, str(src)], None
    if lang == "drift":
        cmd = [str(DRIFT), "build", str(src), "-o", str(exe)]
    else:
        cmd = [str(RUSTC), "-O", "--edition", "2021", str(src), "-o", str(exe)]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        return None, (p.stdout + p.stderr)[-3000:]
    return [str(exe)], None


def run_case(cmd, case, timed=False):
    """Returns dict(out, code, cpu, rss, wall, timeout)."""
    args = json.loads(case.with_suffix(".args").read_text())
    with tempfile.TemporaryDirectory() as tmp:
        tfile = pathlib.Path(tmp) / "time"
        full = ["nice", "-n", "15", "/usr/bin/time", "-q", "-o", str(tfile), "-f", "%U %S %M"] + cmd + args
        t0 = time.time()
        with open(case.with_suffix(".in"), "rb") as fin:
            try:
                p = subprocess.run(full, stdin=fin, capture_output=True, timeout=TIMEOUT, start_new_session=True)
            except subprocess.TimeoutExpired:
                return {"out": "", "code": None, "timeout": True, "wall": TIMEOUT, "cpu": None, "rss": None, "err": ""}
        wall = time.time() - t0
        u = s = rss = None
        try:
            parts = tfile.read_text().split()[-3:]
            u, s, rss = float(parts[0]), float(parts[1]), int(parts[2])
        except Exception:
            pass
    return {"out": p.stdout.decode("utf-8", "replace"), "code": p.returncode, "timeout": False, "wall": wall,
            "cpu": (u or 0) + (s or 0), "rss": rss, "err": p.stderr.decode("utf-8", "replace")[-500:]}


def norm(s):
    return s.replace("\r\n", "\n").rstrip("\n")


def passes(task, case, res):
    exp_out = case.with_suffix(".out").read_text()
    exp_code = int(case.with_suffix(".code").read_text())
    if res["timeout"]:
        return False, f"timed out after {TIMEOUT} s"
    if task == "t12":
        ok = R.t12_check(case.with_suffix(".in").read_bytes(), norm(res["out"]) + "\n", res["code"], exp_out, exp_code)
        return ok, None if ok else describe(exp_out, res, exp_code)
    if res["code"] != exp_code:
        return False, f"expected exit status {exp_code}, got {res['code']}" + (f"; stderr: {res['err'][-300:]}" if res["err"] else "")
    if exp_code != 0:
        return True, None  # error cases: only the exit status is checked
    if task == "t05":
        try:
            ok = json.loads(res["out"]) == json.loads(exp_out)
        except ValueError as e:
            return False, f"output is not valid JSON ({e})"
        return ok, None if ok else "the JSON differs from the expected value"
    if norm(res["out"]) == norm(exp_out):
        return True, None
    return False, describe(exp_out, res, exp_code)


def describe(exp_out, res, exp_code):
    if res["code"] != exp_code:
        return f"expected exit status {exp_code}, got {res['code']}"
    e, g = norm(exp_out).split("\n"), norm(res["out"]).split("\n")
    for i in range(max(len(e), len(g))):
        a = e[i] if i < len(e) else "<no line>"
        b = g[i] if i < len(g) else "<no line>"
        if a != b:
            return f"output line {i + 1}: expected {a[:150]!r}, got {b[:150]!r}"
    return "output differs"


def results_dir(run, task, lang):
    d = EVAL / "results" / run / f"{task}-{lang}"
    d.mkdir(parents=True, exist_ok=True)
    return d


def submit(task, lang, workdir):
    run = os.environ.get("EVAL_RUN", "run1")
    src = pathlib.Path(workdir) / SRC[lang]
    if not src.exists():
        print(f"no {SRC[lang]} in {workdir}")
        return 2
    rd = results_dir(run, task, lang)
    n = len(list(rd.glob("attempt*.json"))) + 1
    if n > MAX_SUBMISSIONS:
        print(f"no submissions left ({MAX_SUBMISSIONS} used)")
        return 2
    shutil.copy(src, rd / f"attempt{n}{src.suffix}")
    cases = sorted((EVAL / "hidden" / task).glob("*.in"))
    lock = open(EVAL / "results" / ".lock", "w")
    print("waiting for the grader...", flush=True) if not try_lock(lock) else None
    fcntl.flock(lock, fcntl.LOCK_EX)  # one grading at a time: the machine has 2 cores
    with tempfile.TemporaryDirectory() as tmp:
        cmd, err = build(lang, src, pathlib.Path(tmp))
        rec = {"attempt": n, "time": time.time(), "cases": len(cases)}
        if err is not None:
            rec.update(passed=0, ok=False, failure="build failed:\n" + err)
        else:
            passed, first, cpu = 0, None, {}
            for c in cases:
                res = run_case(cmd, c)
                ok, why = passes(task, c, res)
                passed += ok
                if "perf" in c.stem:
                    cpu[c.stem] = res["cpu"]
                if not ok and first is None:
                    first = f"case {c.stem}: {why}"
            rec.update(passed=passed, ok=passed == len(cases), failure=first, cpu=cpu)
    (rd / f"attempt{n}.json").write_text(json.dumps(rec))
    print(f"submission {n}/{MAX_SUBMISSIONS}: {rec['passed']}/{len(cases)} hidden tests passed")
    if rec["failure"]:
        print("first failure: " + rec["failure"])
    return 0 if rec["ok"] else 1


def try_lock(f):
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except OSError:
        return False


def final(rd):
    atts = sorted(rd.glob("attempt*.json"), key=lambda p: int(p.stem[7:]))
    return [json.loads(a.read_text()) for a in atts]


def bench(run, repeats=3):
    out = {}
    for rd in sorted((EVAL / "results" / run).iterdir()):
        if not rd.is_dir():
            continue
        task, lang = rd.name.split("-")
        atts = final(rd)
        if not atts or not atts[-1]["ok"]:
            continue
        src = next(rd.glob(f"attempt{atts[-1]['attempt']}.*"))
        with tempfile.TemporaryDirectory() as tmp:
            s = pathlib.Path(tmp) / SRC[lang]
            shutil.copy(src, s)
            cmd, err = build(lang, s, pathlib.Path(tmp))
            for c in sorted((EVAL / "hidden" / task).glob("*perf*.in")):
                rs = [run_case(cmd, c) for _ in range(repeats)]
                cpus = sorted(r["cpu"] for r in rs if r["cpu"] is not None)
                out[f"{task}/{c.stem}/{lang}"] = {"cpu": cpus[len(cpus) // 2] if cpus else None, "rss_kb": max((r["rss"] or 0) for r in rs),
                                                  "ok": all(passes(task, c, r)[0] for r in rs)}
                print(task, c.stem, lang, out[f"{task}/{c.stem}/{lang}"], flush=True)
    (EVAL / "results" / run / "bench.json").write_text(json.dumps(out, indent=1))


def report(run):
    rows = {}
    for rd in sorted((EVAL / "results" / run).iterdir()):
        if not rd.is_dir():
            continue
        task, lang = rd.name.split("-")
        atts = final(rd)
        rows[(task, lang)] = {"attempts": len(atts), "pass1": bool(atts and atts[0]["ok"]), "pass3": any(a["ok"] for a in atts),
                              "first": f"{atts[0]['passed']}/{atts[0]['cases']}" if atts else "-",
                              "loc": len(next(rd.glob(f"attempt{len(atts)}.*")).read_text().splitlines()) if atts else 0}
    print(json.dumps({f"{t}-{l}": v for (t, l), v in rows.items()}, indent=1))


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "submit" and len(sys.argv) == 5 and sys.argv[3] in SRC:
        sys.exit(submit(sys.argv[2], sys.argv[3], sys.argv[4]))
    elif cmd == "bench" and len(sys.argv) >= 3:
        bench(sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 3)
    elif cmd == "report" and len(sys.argv) == 3:
        report(sys.argv[2])
    else:
        print(__doc__)
        sys.exit(2)
