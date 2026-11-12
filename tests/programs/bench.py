import time
def fib(n): return n if n < 2 else fib(n-1) + fib(n-2)
t0=time.time(); f=fib(32); t1=time.time()
counts={}; xs=[]
for i in range(1000000):
    k=i*7919%10007; counts[k]=counts.get(k,0)+1; xs.append(k)
total=sum(sorted(xs)[:1000]); t2=time.time()
s=""
for i in range(200000): s+=f"{i},"
words=len(s.split(",")); t3=time.time()
print(f"fib={f} keys={len(counts)} total={total} words={words}")
print(f"fib {int((t1-t0)*1000)} ms, map+list+sort {int((t2-t1)*1000)} ms, strings {int((t3-t2)*1000)} ms")
