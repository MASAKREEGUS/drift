"""Generates the hidden test cases into eval/hidden/<task>/ (deterministic: fixed seeds).

    python3 eval/gen.py [task...]

Each case is <nn>-<label>.in (stdin), .args (JSON list), .out and .code (from reference.py). Labels containing
"perf" are the cases `grade.py bench` times.
"""
import json, pathlib, random, sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import reference as R

ROOT = pathlib.Path(__file__).parent / "hidden"


def write(task, cases):
    d = ROOT / task
    d.mkdir(parents=True, exist_ok=True)
    for f in d.iterdir():
        f.unlink()
    for i, (label, data, args) in enumerate(cases, 1):
        data = data.encode() if isinstance(data, str) else data
        out, code = getattr(R, task)(data, args)
        base = d / f"{i:02d}-{label}"
        base.with_suffix(".in").write_bytes(data)
        base.with_suffix(".args").write_text(json.dumps(args))
        base.with_suffix(".out").write_text(out)
        base.with_suffix(".code").write_text(str(code))
    print(task, len(cases), "cases")


def g01():
    r = random.Random(1)
    c = [("example", "3 1 4 1 5\n", []), ("single", "42", []), ("neg-even", "-3 -4\n", []), ("even-whole", "1 2 3 5", []),
         ("half-zero", "-1 0 7 9 -5 3", []), ("spaces", "  10\t-20\n\n 30   40\t\t\n50 \n", []), ("minus-zero", "-0 5 9", []),
         ("extremes", "1000000000 -1000000000 999999999", []), ("neg-mean", "-5 -2 0 1", []),
         ("empty", "", []), ("blank", " \n\t\n", []), ("float", "1 2 3.5", []), ("plus", "1 +5 2", []), ("suffix", "12x 3", []),
         ("double-minus", "--1", []), ("word", "7 abc", [])]
    for label, n in (("random", 1001), ("perf-1e6", 1000000)):
        while True:
            vals = [r.randint(-10**9, 10**9) for _ in range(n)]
            text = "\n".join(" ".join(map(str, vals[i:i + 10])) for i in range(0, n, 10)) + "\n"
            if R.t01_ok(text):
                break
        c.append((label, text, []))
    for label, data, _ in c:
        if label not in ("empty", "blank", "float", "plus", "suffix", "double-minus", "word"):
            assert R.t01_ok(data), label
    return c


def g03():
    r = random.Random(3)
    es = "El niño comió piña. ¡Qué rico! El NIÑO y la niña: PIÑA, piña, Piña.\nAñoranza año AÑO años\n"
    de = "Die Straße, die STRASSE, die straße. Groß groß GROSS. Übermut über ÜBER\n"
    el = "ΟΔΟΣ οδός Οδός ΣΑΣ σας Σας. ΑΣ1 ας1 ΌΣΟΣ\n"
    tr = "İstanbul İSTANBUL istanbul ıstanbul\n"
    ru = "Привет, мир! ПРИВЕТ привет Мир мир МИР ёлка ЁЛКА\n"
    mix = "x² X² 2024 ２０２４ 東京 東京 日本語 ǅemal ǆemal Ǆemal don't DON'T won’t 🙂emoji🙂 emoji a_b a-b a.b\n"
    c = [("example", "b a B a, c", ["2"]), ("spanish", es, ["5"]), ("german", de, ["10"]), ("greek-sigma", el, ["10"]),
         ("turkish", tr, ["10"]), ("russian", ru, ["4"]), ("mixed", mix, ["50"]), ("all", es + de + el + tr + ru + mix, ["1000"]),
         ("n-one", es, ["1"]), ("empty-text", "", ["3"]), ("punct-only", "... ,,, !!! ¿¿", ["3"]),
         ("n-zero", es, ["0"]), ("n-missing", es, []), ("n-word", es, ["abc"]), ("n-negative", es, ["-3"])]
    vocab = [w for w in (es + de + el + tr + ru + mix).replace("\n", " ").split(" ") if w] + ["Ünïcödé", "ÇA", "ça", "Ça"]
    words = [r.choice(vocab) for _ in range(600000)]
    big = "".join(w + r.choice([" ", " ", " ", "\n", ", ", ". ", "\t"]) for w in words)
    c.append(("perf-text", big, ["25"]))
    return c


def csv_field(r, quote_all=False):
    alphabet = ["a", "b", "Z", "1", " ", ",", '"', "\n", "\r\n", "ñ", "é", "🙂", "\\", "\t", "\u0001", "/", "日"]
    v = "".join(r.choice(alphabet) for _ in range(r.randint(0, 8)))
    if quote_all or any(x in v for x in ',"\n\r'):
        return '"' + v.replace('"', '""') + '"'
    return v


def g05():
    r = random.Random(5)
    c = [("example", 'name,note\nAna,"says ""hi"", twice"\n', []), ("simple", "a,b,c\n1,2,3\n4,5,6\n", []),
         ("crlf", "a,b\r\n1,2\r\n3,4\r\n", []), ("no-final-newline", "a,b\n1,2\n3,4", []),
         ("quoted-newlines", 'id,text\n1,"line one\nline two"\n2,"with\r\ncrlf"\n', []), ("empty-input", "", []),
         ("header-only", "a,b,c\n", []), ("header-only-no-newline", "a,b,c", []), ("empty-fields", "a,b,c\n,,\n1,,\n", []),
         ("quoted-empty", 'a,b\n"",""\n"""""",x\n', []), ("quoted-header", '"first name","last, name"\nAda,Lovelace\n', []),
         ("one-column", "x\n1\n2\n", []), ("unicode", "ciudad,emoji\nÑuñoa,🙂\n東京,\"日本, 語\"\nBack\\slash,\"tab\there\"\n", []),
         ("control-chars", "k,v\nbell,\u0007\nnul-ish,\u0001x\n", []),
         ("too-many-fields", "a,b\n1,2\n3,4,5\n", []), ("too-few-fields", "a,b\n1,2\n3\n", []),
         ("unterminated", 'a,b\n1,"abc\n', []), ("quote-in-field", 'a,b\n1,ab"c\n', []), ("after-quote", 'a,b\n1,"ab"c\n', []),
         ("short-last-no-newline", "a,b\n1,2\n3", [])]
    for label, rows, cols in (("random", 200, 5), ("perf-big", 60000, 8)):
        head = ",".join(f"col{i}" for i in range(cols))
        body = "".join(",".join(csv_field(r) for _ in range(cols)) + r.choice(["\n", "\r\n"]) for _ in range(rows))
        c.append((label, head + "\n" + body, []))
    return c


def rand_expr(r, depth=0):
    k = r.random()
    if depth > 4 or k < 0.3:
        return str(r.choice([r.randint(0, 9), r.randint(0, 1000), r.randint(0, 10**6)]))
    if k < 0.4:
        return "-" + rand_expr(r, depth + 1)
    if k < 0.55:
        return "(" + rand_expr(r, depth + 1) + ")"
    op = r.choice(["+", "-", "*", "/", "%", "+", "-"])
    sp = r.choice(["", " ", "  ", "\t"])
    return rand_expr(r, depth + 1) + sp + op + sp + rand_expr(r, depth + 1)


def mutate(r, e):
    k = r.randint(0, 5)
    i = r.randint(0, len(e))
    if k == 0:
        return e[:i] + r.choice(["x", "ñ", "#", "１", "."]) + e[i:]
    if k == 1:
        return e[:i] + r.choice(["(", ")", "*", "+"]) + e[i:]
    if k == 2:
        return e[:i]
    if k == 3:
        return e + " /0"
    return e


def ok_expr(line):
    """The task promises no overflow in any value that is evaluated, also before a division by zero."""
    R.OVERFLOW[0] = 0
    try:
        R.eval_line(line)
    except R.ExprError:
        pass
    return not R.OVERFLOW[0]


def g06():
    r = random.Random(6)
    ex = "1 + 2 * 3\n(1 + 2) * -3\n7 / -2\n2 * (3 + 4\n1 + * 2\n4 / (2 - 2)\n1 + ñ\n"
    cdiv = "-7 / 2\n-7 % 2\n7 % -2\n-7 % -2\n7 / -2\n-7 / -2\n0 / 5\n0 % 5\n-1 / 3\n-6 / 3\n6 % 3\n100 % 7 * 2\n"
    prec = "2 + 3 * 4 - 5\n2 * 3 + 4 * 5\n10 - 4 - 3\n100 / 10 / 5\n2 * 3 % 4\n- - -3\n--3\n-(2 + 3) * 4\n((((7))))\n3 - -3\n2*-3\n-2*-3\n"
    big = "1000000000000000000\n999999999999999999 + 1\n3037000499 * 3037000499\n-1000000000000000000 * 9\n9000000000000000000 / -7\n"
    errs = ("1 + (2 * x\n(1))\n)\n1 2\n1 +\n1 + \n5 % 0\n(1/0) + (1/0)\n0/0*\n-\n()\n2*(3+4)*(5-(6/(7-7)))\n1 + 2 ñ\n１\n"
            "ñ + ü\n  12  +  \t\n7 / (3 - 3) / 0\n(5 % (2 - 2)) + (1 / 0)\n1 / 0 + )\n3 4 +\n((1)\n12a\n")
    blank = "\n   \n1+1\n\t\n\n2*2\n"
    c = [("examples", ex, []), ("c-division", cdiv, []), ("precedence", prec, []), ("big-numbers", big, []), ("errors", errs, []),
         ("blank-lines", blank, []), ("no-final-newline", "1+2\n3*4", []), ("empty", "", [])]
    for label, n in (("random", 400), ("perf-lines", 100000)):
        lines = []
        while len(lines) < n:
            e = rand_expr(r)
            if r.random() < 0.3:
                e = mutate(r, e)
            if ok_expr(e) and e.strip(" \t"):
                lines.append(e)
        c.append((label, "\n".join(lines) + "\n", []))
    return c


def g10():
    r = random.Random(10)
    c = [("example", "3 3\n0 1 5\n1 2 1\n0 2 10\n0 2\n", []), ("unreachable", "4 2\n0 1 3\n2 3 4\n0 3\n", []),
         ("same-node", "3 2\n0 1 1\n1 2 1\n1 1\n", []), ("single-node", "1 0\n0 0\n", []),
         ("loops-parallel", "3 5\n0 0 1\n0 1 7\n0 1 3\n1 1 2\n1 2 4\n0 2\n", []), ("directed", "3 2\n1 0 1\n2 1 1\n0 2\n", []),
         ("big-weights", "3 2\n0 1 1000000000\n1 2 1000000000\n0 2\n", [])]

    def graph(n, m, label):
        while True:
            edges = [(r.randrange(n), r.randrange(n), r.randint(1, 10**9)) for _ in range(m)]
            s, t = r.randrange(n), r.randrange(n)
            text = f"{n} {m}\n" + "".join(f"{u} {v} {w}\n" for u, v, w in edges) + f"{s} {t}\n"
            R.t10.unique = True
            out, _ = R.t10(text.encode(), [])
            if out != "no path\n" and R.t10.unique:
                return (label, text, [])

    c.append(graph(50, 200, "small-random"))
    c.append(graph(2000, 10000, "medium-random"))
    n = 200000
    perm = list(range(n))
    r.shuffle(perm)
    chain = [(perm[i], perm[i + 1], r.randint(1, 1000)) for i in range(n - 1)]
    chain += [(perm[i], perm[i + 2], 10**9) for i in range(0, n - 2, 97)]
    c.append(("perf-long-path", f"{n} {len(chain)}\n" + "".join(f"{u} {v} {w}\n" for u, v, w in chain) + f"{perm[0]} {perm[-1]}\n", []))
    c.append(graph(200000, 1000000, "perf-random"))
    return c


def g12():
    r = random.Random(12)
    c = [("example", "shirt -> tie\ntie -> jacket\npants -> shoes\nsocks\n", []), ("chain", "c -> b\nb -> a\n", []),
         ("diamond", "top -> left\ntop -> right\nleft -> bottom\nright -> bottom\n", []),
         ("byte-order", "a\nB\na_\na1\nA\n_x\nZ9\nz\n", []), ("spacing", "a->b\n  b   ->   c  \n\tc\t->\td\n\nd\n", []),
         ("duplicates", "a -> b\na -> b\nb -> c\na -> b\n", []), ("isolated", "q\nw\ne\nr\n", []),
         ("cycle-3", "a -> b\nb -> c\nc -> a\n", []), ("self-loop", "a -> a\n", []),
         ("cycle-with-dag", "aa -> bb\nm -> n\nn -> o\no -> m\nzz -> m\nb1 -> aa\n", []), ("two-cycle", "x -> y\ny -> x\n", []),
         ("empty", "", [])]
    names = [f"n{i}" for i in range(100000)]
    edges = set()
    while len(edges) < 300000:
        a, b = sorted(r.sample(range(100000), 2))
        edges.add((names[a], names[b]))
    lines = [f"{a} -> {b}" for a, b in edges] + names[::7]
    r.shuffle(lines)
    c.append(("perf-dag", "\n".join(lines) + "\n", []))
    cyc = r.sample(range(100000), 50000)
    lines2 = lines[:200000] + [f"c{cyc[i]} -> c{cyc[(i + 1) % len(cyc)]}" for i in range(len(cyc))]
    r.shuffle(lines2)
    c.append(("perf-long-cycle", "\n".join(lines2) + "\n", []))
    return c


def full_grid(r):
    g = list("." * 81)
    digits = list("123456789")

    def rec(i):
        if i == 81:
            return True
        row, col = i // 9, i % 9
        r.shuffle(digits)
        for d in list(digits):
            if all(g[row * 9 + k] != d for k in range(9)) and all(g[k * 9 + col] != d for k in range(9)) and all(
                g[(row // 3 * 3 + a) * 9 + col // 3 * 3 + b] != d for a in range(3) for b in range(3)):
                g[i] = d
                if rec(i + 1):
                    return True
                g[i] = "."
        return False

    rec(0)
    return "".join(g)


def make_puzzle(r):
    g = list(full_grid(r))
    for i in r.sample(range(81), 81):
        keep = g[i]
        g[i] = "."
        if len(R.sudoku_solve("".join(g), 2)) != 1:
            g[i] = keep
    return "".join(g)


def g13():
    r = random.Random(13)
    inkala = "8..........36......7..9.2...5...7.......457.....1...3...1....68..85...1..9....4.."
    anti_brute = "..............3.85..1.2.......5.7.....4...1...9.......5......73..2.1........4...9"
    for p in (inkala, anti_brute):
        assert len(R.sudoku_solve(p, 2)) == 1, p
    gen = [make_puzzle(r) for _ in range(60)]
    solved = R.sudoku_solve(gen[0], 1)[0]
    unsolvable = None  # change one given so that no solution exists although no row/column/box conflicts
    for i in range(81):
        if gen[0][i] == "." or unsolvable:
            continue
        for d in "123456789":
            t = gen[0][:i] + d + gen[0][i + 1:]
            peers = [j for j in range(81) if j != i and (j // 9 == i // 9 or j % 9 == i % 9 or (j // 27 == i // 27 and j % 9 // 3 == i % 9 // 3))]
            if d != gen[0][i] and all(t[j] != d for j in peers) and not R.sudoku_solve(t, 1):
                unsolvable = t
                break
    assert not R.sudoku_solve(unsolvable, 1)
    conflict = "11" + "." * 79
    zeros = gen[1].replace(".", "0")
    c = [("easy", gen[2] + "\n", []), ("zeros", zeros + "\n", []), ("solved", solved + "\n", []), ("conflict", conflict + "\n", []),
         ("no-solution", unsolvable + "\n", []), ("invalid", gen[3][:80] + "\n" + gen[3] + "5\n" + gen[3][:40] + "x" + gen[3][41:] + "\n", []),
         ("mixed", "\n".join([gen[4], conflict, "short", gen[5].replace(".", "0"), inkala]) + "\n", []), ("no-final-newline", gen[6], []),
         ("hard-inkala", inkala + "\n", []), ("perf-anti-brute", anti_brute + "\n", []),
         ("perf-many", "\n".join(gen[7:] + [inkala, anti_brute] * 3) + "\n", [])]
    return c


def g25():
    return [("zero", "0\n", []), ("one", "1", []), ("two", "2\n", []), ("three", "3\n", []), ("ten", "10\n", []), ("hundred", "  100  \n", []),
            ("prime-bound", "1000003\n", []), ("ten-million", "10000000\n", []), ("perf-50m", "50000000\n", []), ("perf-100m", "100000000\n", [])]


GENS = {"t01": g01, "t03": g03, "t05": g05, "t06": g06, "t10": g10, "t12": g12, "t13": g13, "t25": g25}

if __name__ == "__main__":
    for t in sys.argv[1:] or GENS:
        write(t, GENS[t]())
