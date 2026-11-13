"""Reference solutions used to produce the expected outputs of the hidden tests: f(stdin: bytes, args) -> (stdout, exit)."""
import heapq, json, re, unicodedata
from fractions import Fraction


def t01(data, args):
    toks = data.decode().split()
    if not toks or any(not re.fullmatch(r"-?[0-9]+", t) for t in toks):
        return "", 1
    v = sorted(int(t) for t in toks)
    n = len(v)
    q = Fraction(sum(v) * 100, n)
    r = (q + Fraction(1, 2)).__floor__() if q >= 0 else -((-q + Fraction(1, 2)).__floor__())
    mean = ("-" if r < 0 else "") + f"{abs(r) // 100}.{abs(r) % 100:02d}"
    if n % 2:
        med = str(v[n // 2])
    else:
        s = v[n // 2 - 1] + v[n // 2]
        med = str(s // 2) if s % 2 == 0 else ("-" if s < 0 else "") + f"{abs(s) // 2}.5"
    return f"min {v[0]}\nmax {v[-1]}\nmean {mean}\nmedian {med}\n", 0


def t01_ok(data):  # generator guard: no ties, not in (-0.005, 0)
    v = [int(t) for t in data.split()]
    q = Fraction(sum(v) * 100, len(v))
    return q.denominator != 2 and not (Fraction(-1, 2) < q < 0)


def t03(data, args):
    if not args or not re.fullmatch(r"[0-9]+", args[0]) or int(args[0]) == 0:
        return "", 2
    n = int(args[0])
    counts, cur = {}, []
    for ch in data.decode("utf-8") + " ":
        if unicodedata.category(ch)[0] in "LN":
            cur.append(ch)
        elif cur:
            w = "".join(cur).lower()
            counts[w] = counts.get(w, 0) + 1
            cur = []
    top = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]
    return "".join(f"{c} {w}\n" for w, c in top), 0


class CsvError(Exception):
    pass


def parse_csv(s):
    recs, rec, i, n = [], [], 0, len(s)
    if n == 0:
        return recs
    while True:
        if i < n and s[i] == '"':
            i += 1
            buf = []
            while True:
                if i >= n:
                    raise CsvError("unterminated")
                if s[i] == '"':
                    if i + 1 < n and s[i + 1] == '"':
                        buf.append('"')
                        i += 2
                        continue
                    i += 1
                    break
                buf.append(s[i])
                i += 1
            if i < n and s[i] != "," and s[i] != "\n" and not s.startswith("\r\n", i):
                raise CsvError("after quote")
            field = "".join(buf)
        else:
            j = i
            while j < n and s[j] != "," and s[j] != "\n" and not s.startswith("\r\n", j):
                if s[j] == '"':
                    raise CsvError("quote in field")
                j += 1
            field, i = s[i:j], j
        rec.append(field)
        if i >= n:
            recs.append(rec)
            break
        if s[i] == ",":
            i += 1
            continue
        i += 2 if s[i] == "\r" else 1
        recs.append(rec)
        rec = []
        if i >= n:
            break
    return recs


def t05(data, args):
    try:
        recs = parse_csv(data.decode("utf-8"))
    except CsvError:
        return "", 1
    if not recs:
        return "[]\n", 0
    head = recs[0]
    if any(len(r) != len(head) for r in recs[1:]):
        return "", 1
    return json.dumps([dict(zip(head, r)) for r in recs[1:]], ensure_ascii=False) + "\n", 0


OVERFLOW = [0]  # set when any value evaluated so far left signed 64-bit (also before a division by zero)


class ExprError(Exception):
    def __init__(self, col, msg):
        self.col, self.msg = col, msg


def cdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b >= 0) else -q


def eval_line(line):
    toks = []
    i = 0
    while i < len(line):
        c = line[i]
        if c in " \t":
            i += 1
        elif "0" <= c <= "9":
            j = i
            while j < len(line) and "0" <= line[j] <= "9":
                j += 1
            toks.append(("n", int(line[i:j]), i + 1))
            i = j
        elif c in "+-*/%()":
            toks.append((c, None, i + 1))
            i += 1
        else:
            raise ExprError(i + 1, "unexpected character")
    end = len(line) + 1
    pos = [0]

    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None

    def fail():
        t = peek()
        raise ExprError(t[2], "unexpected token") if t else ExprError(end, "unexpected end")

    def expr():
        node = term()
        while peek() and peek()[0] in "+-":
            op = toks[pos[0]]
            pos[0] += 1
            node = (op[0], op[2], node, term())
        return node

    def term():
        node = unary()
        while peek() and peek()[0] in "*/%":
            op = toks[pos[0]]
            pos[0] += 1
            node = (op[0], op[2], node, unary())
        return node

    def unary():
        t = peek()
        if t and t[0] == "-":
            pos[0] += 1
            return ("neg", t[2], unary())
        return primary()

    def primary():
        t = peek()
        if t and t[0] == "n":
            pos[0] += 1
            return ("n", t[1])
        if t and t[0] == "(":
            pos[0] += 1
            node = expr()
            if not (peek() and peek()[0] == ")"):
                fail()
            pos[0] += 1
            return node
        fail()

    tree = expr()
    if peek():
        fail()
    big = OVERFLOW
    big[0] = 0

    def ev(node):
        k = node[0]
        if k == "n":
            v = node[1]
        elif k == "neg":
            v = -ev(node[2])
        else:
            a, b = ev(node[2]), ev(node[3])
            if k in "/%" and b == 0:
                raise ExprError(node[1], "division by zero")
            v = {"+": lambda: a + b, "-": lambda: a - b, "*": lambda: a * b, "/": lambda: cdiv(a, b), "%": lambda: a - b * cdiv(a, b)}[k]()
        if abs(v) >= 2**63 - 1:
            big[0] = 1
        return v

    v = ev(tree)
    return v, big[0]


def t06(data, args):
    out = []
    for line in data.decode("utf-8").split("\n"):
        if line.strip(" \t") == "":
            continue
        try:
            out.append(str(eval_line(line)[0]))
        except ExprError as e:
            out.append(f"error at {e.col}: {e.msg}")
    return "".join(x + "\n" for x in out), 0


def t10(data, args):
    it = iter(data.split())
    n, m = int(next(it)), int(next(it))
    adj = [[] for _ in range(n)]
    for _ in range(m):
        u, v, w = int(next(it)), int(next(it)), int(next(it))
        adj[u].append((v, w))
    s, t = int(next(it)), int(next(it))
    dist = [None] * n
    prev = [-1] * n
    ways = [0] * n
    dist[s], ways[s] = 0, 1
    h = [(0, s)]
    done = [False] * n
    while h:
        d, u = heapq.heappop(h)
        if done[u]:
            continue
        done[u] = True
        for v, w in adj[u]:
            nd = d + w
            if dist[v] is None or nd < dist[v]:
                dist[v], prev[v], ways[v] = nd, u, ways[u]
                heapq.heappush(h, (nd, v))
            elif nd == dist[v] and not done[v]:
                ways[v] = min(2, ways[v] + ways[u])
    if dist[t] is None:
        return "no path\n", 0
    path = [t]
    while path[-1] != s:
        path.append(prev[path[-1]])
    t10.unique = ways[t] == 1
    return f"cost {dist[t]}\npath {' '.join(map(str, reversed(path)))}\n", 0


EDGE = re.compile(r"^\s*([A-Za-z0-9_]+)\s*->\s*([A-Za-z0-9_]+)\s*$")


def parse_graph(text):
    nodes, edges = set(), set()
    for line in text.split("\n"):
        if not line.strip():
            continue
        m = EDGE.match(line)
        if m:
            nodes.update(m.groups())
            edges.add(m.groups())
        else:
            nodes.add(line.strip())
    return nodes, edges


def t12(data, args):
    nodes, edges = parse_graph(data.decode())
    succ = {x: [] for x in nodes}
    indeg = {x: 0 for x in nodes}
    for a, b in edges:
        succ[a].append(b)
        indeg[b] += 1
    h = [x.encode() for x in nodes if indeg[x] == 0]
    heapq.heapify(h)
    order = []
    while h:
        x = heapq.heappop(h).decode()
        order.append(x)
        for y in succ[x]:
            indeg[y] -= 1
            if indeg[y] == 0:
                heapq.heappush(h, y.encode())
    if len(order) == len(nodes):
        return "".join(x + "\n" for x in order), 0
    left = {x for x in nodes if indeg[x] > 0}
    pred = {}
    for a, b in edges:
        if a in left and b in left:
            pred.setdefault(b, []).append(a)
    x = min(left)
    seen, pos = [], {}
    while x not in pos:  # every remaining node has a predecessor among the remaining ones: walk backwards
        pos[x] = len(seen)
        seen.append(x)
        x = min(pred[x])
    cyc = seen[pos[x]:][::-1]
    return "cycle: " + " -> ".join(cyc + [cyc[0]]) + "\n", 1


def t12_check(data, out, code, exp_out, exp_code):
    """A cycle may be any cycle; an order must match exactly."""
    if exp_code == 0:
        return code == 0 and out.rstrip("\n") == exp_out.rstrip("\n")
    if code != 1:
        return False
    lines = [l for l in out.split("\n") if l]
    if len(lines) != 1 or not lines[0].startswith("cycle: "):
        return False
    names = lines[0][len("cycle: "):].split(" -> ")
    _, edges = parse_graph(data.decode())
    return len(names) >= 2 and names[0] == names[-1] and len(set(names[:-1])) == len(names) - 1 and all((a, b) in edges for a, b in zip(names, names[1:]))


def sudoku_solve(p, limit=2):
    """Bitmask backtracking with most-constrained cell first. Returns up to `limit` solutions."""
    cells = [0 if ch in ".0" else int(ch) for ch in p]
    rows, cols, boxes = [0] * 9, [0] * 9, [0] * 9
    for i, v in enumerate(cells):
        if v:
            b = 1 << v
            r, c, x = i // 9, i % 9, (i // 27) * 3 + (i % 9) // 3
            if rows[r] & b or cols[c] & b or boxes[x] & b:
                return []
            rows[r] |= b
            cols[c] |= b
            boxes[x] |= b
    sols = []

    def rec():
        best, bm, bc = -1, 0, 10
        for i in range(81):
            if not cells[i]:
                r, c, x = i // 9, i % 9, (i // 27) * 3 + (i % 9) // 3
                m = ~(rows[r] | cols[c] | boxes[x]) & 0x3FE
                k = bin(m).count("1")
                if k < bc:
                    best, bm, bc = i, m, k
                    if k <= 1:
                        break
        if best < 0:
            sols.append("".join(map(str, cells)))
            return len(sols) >= limit
        r, c, x = best // 9, best % 9, (best // 27) * 3 + (best % 9) // 3
        while bm:
            b = bm & -bm
            bm ^= b
            v = b.bit_length() - 1
            cells[best] = v
            rows[r] |= b
            cols[c] |= b
            boxes[x] |= b
            if rec():
                return True
            rows[r] ^= b
            cols[c] ^= b
            boxes[x] ^= b
        cells[best] = 0
        return False

    rec()
    return sols


def t13(data, args):
    lines = data.decode().split("\n")
    if lines[-1] == "":
        lines.pop()  # the end of the last line is not another puzzle
    out = []
    for line in lines:
        line = line[:-1] if line.endswith("\r") else line
        if len(line) != 81 or any(ch not in "123456789.0" for ch in line):
            out.append("invalid")
            continue
        s = sudoku_solve(line, 1)
        out.append(s[0] if s else "no solution")
    return "".join(x + "\n" for x in out), 0


def t25(data, args):
    n = int(data.split()[0])
    if n < 3:
        return ("0" if n <= 2 else "1") + "\n", 0
    sieve = bytearray([1]) * n
    sieve[0] = sieve[1] = 0
    for i in range(2, int(n**0.5) + 1):
        if sieve[i]:
            sieve[i * i :: i] = bytes(len(range(i * i, n, i)))
    return f"{sum(sieve)}\n", 0
