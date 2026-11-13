import sys, re

BAD = re.compile(r'[^0-9 \t+\-*/%()]')
TOK = re.compile(r'[0-9]+|[-+*/%()]')
PREC = {'+': 1, '-': 1, '*': 2, '/': 2, '%': 2}


def solve(line):
    m = BAD.search(line)
    if m:
        return "error at %d: unexpected character" % (m.start() + 1)
    out = []   # RPN: ints, or (op, col)
    ops = []   # entries: (kind, col) kind in '(', 'u', '+','-','*','/','%'
    expect_operand = True
    for t in TOK.finditer(line):
        s = t.group()
        col = t.start() + 1
        c = s[0]
        if expect_operand:
            if c == '-':
                ops.append(('u', col))
            elif c == '(':
                ops.append(('(', col))
            elif c <= '9' and c >= '0':
                out.append(int(s))
                expect_operand = False
            else:
                return "error at %d: unexpected token" % col
        else:
            if c in PREC:
                p = PREC[c]
                while ops:
                    k = ops[-1][0]
                    if k == 'u' or (k != '(' and PREC[k] >= p):
                        out.append(ops.pop())
                    else:
                        break
                ops.append((c, col))
                expect_operand = True
            elif c == ')':
                while ops and ops[-1][0] != '(':
                    out.append(ops.pop())
                if not ops:
                    return "error at %d: unexpected token" % col
                ops.pop()
            else:
                return "error at %d: unexpected token" % col
    if expect_operand:
        return "error at %d: unexpected end" % (len(line) + 1)
    while ops:
        o = ops.pop()
        if o[0] == '(':
            return "error at %d: unexpected end" % (len(line) + 1)
        out.append(o)
    st = []
    push = st.append
    pop = st.pop
    for x in out:
        if type(x) is int:
            push(x)
            continue
        k = x[0]
        if k == 'u':
            st[-1] = -st[-1]
            continue
        b = pop()
        a = st[-1]
        if k == '+':
            st[-1] = a + b
        elif k == '-':
            st[-1] = a - b
        elif k == '*':
            st[-1] = a * b
        else:
            if b == 0:
                return "error at %d: division by zero" % x[1]
            q = abs(a) // abs(b)
            if (a < 0) != (b < 0):
                q = -q
            st[-1] = q if k == '/' else a - b * q
    return str(st[0])


def main():
    data = sys.stdin.buffer.read().decode('utf-8', errors='surrogateescape')
    res = []
    for line in data.split('\n'):
        if line.strip(' \t') == '':
            continue
        res.append(solve(line))
    if res:
        sys.stdout.write('\n'.join(res) + '\n')


main()
