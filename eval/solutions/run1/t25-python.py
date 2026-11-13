import sys
from math import isqrt


def count_primes(n):
    if n <= 2:
        return 0
    if n <= 3:
        return 1
    m = n // 2  # odd numbers < n: indices 0..m-1, index i -> 2i+1
    r = isqrt(n - 1)
    # small sieve of odd primes up to r
    sm = bytearray([1]) * (r // 2 + 1)
    sm[0] = 0
    for i in range(1, (isqrt(r) - 1) // 2 + 1):
        if sm[i]:
            p = 2 * i + 1
            s = p * p // 2
            sm[s::p] = bytes(len(range(s, len(sm), p)))
    primes = [2 * i + 1 for i in range(1, len(sm)) if sm[i] and 2 * i + 1 <= r]
    seg_size = 1 << 18
    zeros = memoryview(bytes(seg_size))
    one = bytes([1]) * seg_size
    starts = [p * p // 2 for p in primes]
    total = 1  # the prime 2
    lo = 0
    seg = bytearray(seg_size)
    np_ = len(primes)
    active = 0
    while lo < m:
        hi = min(lo + seg_size, m)
        size = hi - lo
        if size == seg_size:
            seg[:] = one
        else:
            seg = bytearray(one[:size])
        if lo == 0:
            seg[0] = 0
        while active < np_ and primes[active] * primes[active] // 2 < hi:
            active += 1
        for k in range(active):
            s = starts[k]
            if s >= hi:
                continue
            p = primes[k]
            a = s - lo
            c = (size - 1 - a) // p + 1
            seg[a::p] = zeros[:c]
            starts[k] = s + c * p
        total += seg.count(1)
        lo = hi
    return total


def main():
    data = sys.stdin.read().split()
    n = int(data[0]) if data else 0
    sys.stdout.write(str(count_primes(n)) + "\n")


main()
