import sys

def build():
    units = []
    for r in range(9):
        units.append(tuple(r * 9 + c for c in range(9)))
    for c in range(9):
        units.append(tuple(r * 9 + c for r in range(9)))
    for br in range(3):
        for bc in range(3):
            units.append(tuple((br * 3 + r) * 9 + bc * 3 + c for r in range(3) for c in range(3)))
    peers = []
    for i in range(81):
        s = set()
        for u in units:
            if i in u:
                s.update(u)
        s.discard(i)
        peers.append(tuple(sorted(s)))
    return tuple(units), tuple(peers)

UNITS, PEERS = build()


def propagate(c, stack):
    units = UNITS
    peers = PEERS
    while True:
        while stack:
            i = stack.pop()
            b = c[i]
            for p in peers[i]:
                m = c[p]
                if m & b:
                    m ^= b
                    if not m:
                        return False
                    c[p] = m
                    if not (m & (m - 1)):
                        stack.append(p)
        for u in units:
            once = 0
            twice = 0
            for i in u:
                m = c[i]
                twice |= once & m
                once |= m
            if once != 511:
                return False
            only = once & ~twice
            if only:
                for i in u:
                    m = c[i] & only
                    if m:
                        if m & (m - 1):
                            return False
                        if c[i] != m:
                            c[i] = m
                            stack.append(i)
            if stack:
                break
        if not stack:
            return True


def search(c):
    best = -1
    bc = 10
    for i in range(81):
        m = c[i]
        if m & (m - 1):
            n = m.bit_count()
            if n < bc:
                bc = n
                best = i
                if n == 2:
                    break
    if best < 0:
        return c
    if bc > 2:
        for u in UNITS:
            once = twice = thrice = 0
            for i in u:
                m = c[i]
                thrice |= twice & m
                twice |= once & m
                once |= m
            ex2 = twice & ~thrice
            if ex2:
                b = ex2 & -ex2
                cells = [i for i in u if c[i] & b]
                for i in cells:
                    nc = c[:]
                    nc[i] = b
                    if propagate(nc, [i]):
                        r = search(nc)
                        if r is not None:
                            return r
                return None
    m = c[best]
    while m:
        b = m & -m
        m ^= b
        nc = c[:]
        nc[best] = b
        if propagate(nc, [best]):
            r = search(nc)
            if r is not None:
                return r
    return None


VALID = set("123456789.0")


def solve_line(line):
    if len(line) != 81 or not set(line) <= VALID:
        return "invalid"
    c = [511] * 81
    stack = []
    for i, ch in enumerate(line):
        if ch != '.' and ch != '0':
            c[i] = 1 << (ord(ch) - 49)
            stack.append(i)
    if not propagate(c, stack):
        return "no solution"
    r = search(c)
    if r is None:
        return "no solution"
    return ''.join(chr(48 + m.bit_length()) for m in r)


def main():
    data = sys.stdin.buffer.read().decode('latin-1')
    lines = data.split('\n')
    if lines and lines[-1] == '':
        lines.pop()
    out = []
    for line in lines:
        if line.endswith('\r'):
            line = line[:-1]
        out.append(solve_line(line))
    sys.stdout.write('\n'.join(out) + ('\n' if out else ''))


main()
