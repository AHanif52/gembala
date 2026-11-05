# Ukur daemon (TECH pekerjaan 0.6): RAM 4 pane, latensi, throughput.
# usage: cargo build --release -p gembala && python3 -I scripts/ukur.py target/release/gembala $TMPDIR/gbl-ukur
import base64, json, os, socket, subprocess, sys, time, shutil
bin_, R = sys.argv[1], sys.argv[2]
shutil.rmtree(R, ignore_errors=True); os.mkdir(R, 0o700)
r, w = os.pipe()
p = subprocess.Popen([bin_, "daemon", "--secret-fd", str(w)], env={**os.environ, "GEMBALA_RUNDIR": R}, pass_fds=(w,))
os.close(w); secret = os.fdopen(r).readline().strip()
s = socket.socket(socket.AF_UNIX); s.connect(f"{R}/ui.sock"); s.settimeout(20); f = s.makefile("rwb")
nid = [0]; ev = []
def call(m, params={}):
    nid[0] += 1; f.write((json.dumps({"id": nid[0], "method": m, "params": params}) + "\n").encode()); f.flush()
    while True:
        v = json.loads(f.readline())
        if v.get("id") == nid[0]: return v
        ev.append(v)
def nextev():
    return ev.pop(0) if ev else json.loads(f.readline())
call("hello", {"proto": 1, "secret": secret})
panes = [call("pane.create", {"rows": 40, "cols": 120})["result"]["pane"] for _ in range(4)]
time.sleep(3)
rss = lambda: int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(p.pid)]))
print(f"daemon 4 pane shell: RSS {rss()/1024:.1f} MB")
fp = subprocess.run(["footprint", "-p", str(p.pid)], capture_output=True, text=True).stdout
print("daemon footprint:", [l.split("Footprint:")[1].strip() for l in fp.splitlines() if "Footprint:" in l][:1])
# latency: a `cat` pane (no interactive shell), write 1 byte, wait for the echo back
pc = call("pane.create", {"cmd": "stty raw -echo; cat", "rows": 24, "cols": 80})["result"]["pane"]
call("pane.attach", {"pane": pc}); time.sleep(1.5); ev.clear()
lat = []
for i in range(50):
    t = time.perf_counter(); call("pane.write", {"pane": pc, "data": base64.b64encode(b"x").decode()})
    while True:
        e = nextev()
        if e.get("event") == "pane.output" and b"x" in base64.b64decode(e["params"]["data"]): break
    lat.append((time.perf_counter() - t) * 1000)
lat.sort(); print(f"latensi ketik->echo lewat daemon (50x): median {lat[25]:.2f} ms, p95 {lat[47]:.2f} ms")
# throughput: 50 MB keluaran
pt = call("pane.create", {"cmd": "head -c 52428800 /dev/zero | tr '\\0' 'a' | fold -w 100", "rows": 40, "cols": 120})["result"]["pane"]
import threading
peak = [0]; stop = [False]
def sample():
    while not stop[0]:
        peak[0] = max(peak[0], rss()); time.sleep(0.2)
th = threading.Thread(target=sample); th.start()
t = time.perf_counter(); call("pane.attach", {"pane": pt}); n = 0
tag = ('"pane":%d' % pt).encode()
while True:
    l = f.readline()
    if b'pane.exit' in l and tag in l: break
    if b'pane.output' in l and tag in l: n += (len(l) - 60) * 3 // 4
dt = time.perf_counter() - t; stop[0] = True; th.join()
print(f"throughput: ~{n/1e6:.1f} MB dalam {dt:.2f} s = {n/1e6/dt:.1f} MB/s (base64 JSON), puncak RSS daemon {peak[0]/1024:.1f} MB, sesudah {rss()/1024:.1f} MB")
p.kill(); shutil.rmtree(R, ignore_errors=True)
