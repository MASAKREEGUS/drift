import sys
import re

_TOKEN = re.compile(rb'-?[0-9]+')


def fail(msg):
    sys.stderr.write("error: " + msg + "\n")
    sys.exit(1)


def main():
    data = sys.stdin.buffer.read()
    tokens = data.split()
    if not tokens:
        fail("no integers in input")
    fm = _TOKEN.fullmatch
    for t in tokens:
        if fm(t) is None:
            fail("invalid token: %r" % t)
    vals = list(map(int, tokens))
    n = len(vals)
    vals.sort()
    s = sum(vals)

    # mean rounded to 2 decimals, exact integer arithmetic (never a tie)
    r = (200 * s + n) // (2 * n)
    sign = "-" if r < 0 else ""
    ar = abs(r)
    mean = "%s%d.%02d" % (sign, ar // 100, ar % 100)

    if n % 2 == 1:
        median = str(vals[n // 2])
    else:
        t = vals[n // 2 - 1] + vals[n // 2]
        if t % 2 == 0:
            median = str(t // 2)
        else:
            median = "%s%d.5" % ("-" if t < 0 else "", abs(t) // 2)

    sys.stdout.write("min %d\nmax %d\nmean %s\nmedian %s\n" % (vals[0], vals[-1], mean, median))


main()
