# Throwaway: measures workerd running handler.js against kernel.js (ledger #3629).
# usage: python3 -I measure.py <path-to-workerd>
import http.client, os, statistics as st, subprocess, sys, time
W = sys.argv[1]; HERE = os.path.dirname(os.path.abspath(__file__)); PORT = 18081

def config(jit):
    out = subprocess.run([sys.executable, "-I", os.path.join(HERE, "gen_config.py"), "granted", "deny", jit, str(PORT)],
                         capture_output=True, text=True, check=True).stdout
    path = os.path.join(HERE, f"m-{jit}.capnp"); open(path, "w").write(out); return path

def get(conn, t):
    conn.request("GET", "/" + t); r = conn.getresponse(); body = r.read(); return r.status, body

def run(jit):
    cfg = config(jit); colds = []
    for _ in range(7):
        t0 = time.perf_counter(); p = subprocess.Popen([W, "serve", cfg], cwd=HERE, stderr=subprocess.DEVNULL)
        while True:
            try:
                c = http.client.HTTPConnection("127.0.0.1", PORT); s, b = get(c, "urn:js:hello"); break
            except OSError: time.sleep(0.0005)
        colds.append((time.perf_counter() - t0) * 1e3); assert b == b"hello, world", b
        c.close(); p.terminate(); p.wait()
    p = subprocess.Popen([W, "serve", cfg], cwd=HERE, stderr=subprocess.DEVNULL); time.sleep(1.5)
    c = http.client.HTTPConnection("127.0.0.1", PORT); res = {}
    for t, n in [("urn:js:noop", 2000), ("urn:js:hello", 2000), ("urn:js:greet", 2000), ("urn:js:loop", 15)]:
        xs = []
        for _ in range(n):
            t0 = time.perf_counter(); s, b = get(c, t); xs.append((time.perf_counter() - t0) * 1e6)
        res[t] = st.median(xs)
    inner = {w: float(get(c, f"bench:{w}:20000")[1]) * 1e3 for w in ("hello", "greet")}
    rss = int(subprocess.run(["ps", "-o", "rss=", "-p", str(p.pid)], capture_output=True, text=True).stdout) * 1024
    p.terminate(); p.wait()
    print(f"{jit:8} cold(spawn->first hello) median {st.median(colds):7.2f}ms  min {min(colds):7.2f}ms")
    for t, v in res.items(): print(f"{jit:8} warm HTTP median {t:14} {v:10.1f}us")
    print(f"{jit:8} loop minus noop {(res['urn:js:loop'] - res['urn:js:noop'])/1e3:8.2f}ms")
    for w, v in inner.items(): print(f"{jit:8} in-isolate per {w:6} {v:8.2f}us (20000 calls, Date.now)")
    print(f"{jit:8} RSS after warm runs {rss/1e6:7.1f} MB")

print(f"# {subprocess.run([W, '--version'], capture_output=True, text=True).stdout.strip()}  {os.uname().sysname} {os.uname().machine}")
for jit in ("jit", "jitless"): run(jit)
