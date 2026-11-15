# Dropped UI connection with the real debug app: the daemon keeps running, only the UI's socket dies (and new
# connections fail for a few seconds). The panes must NOT turn into dead panes (negative ids) and no second pane may be
# created; the UI reconnects and attaches again. The UI talks to the daemon through a proxy socket this script can cut.
# Compare lost_check.py: there the daemon itself is killed and the dead-pane cards must appear.
# usage: npm run tauri build -- --debug && python3 -I scripts/drop_check.py target/debug/bundle/macos/Gembala.app
import json, os, shutil, socket, subprocess, sys, threading, time

app = sys.argv[1]
B = os.path.join(os.environ["TMPDIR"], "gdrop")  # short: socket paths are limited to ~100 bytes
D, P = os.path.join(B, "d"), os.path.join(B, "p")  # real daemon run folder / the UI's (proxy) run folder
CFG = os.path.join(B, "cfg", "config.toml")
LAYOUT = os.path.join(B, "cfg", "layout.json")

def pids(pattern):
    out = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True).stdout
    return {int(p) for p in out.split()}

def wait(cond, secs):
    t0 = time.time()
    while time.time() - t0 < secs:
        if cond():
            return True
        time.sleep(0.2)
    return False

def layout():
    with open(LAYOUT) as fh:
        return json.load(fh)

def ids(L):
    def walk(n):
        return [n["pane"]] if "pane" in n else walk(n["a"]) + walk(n["b"])
    return sorted(p for w in L["workspaces"] for t in w["tabs"] for p in walk(t["tree"]))

class Proxy:
    """P/ui.sock → D/ui.sock; `cut(secs)` closes every connection and refuses new ones for `secs`."""
    def __init__(self):
        self.conns, self.blocked_until, self.hellos, self.attaches_after_cut, self.cut_at = [], 0, 0, 0, None
        self.l = socket.socket(socket.AF_UNIX)
        self.l.bind(os.path.join(P, "ui.sock"))
        self.l.listen(8)
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            c, _ = self.l.accept()
            if time.time() < self.blocked_until:
                c.close()
                continue
            try:
                u = socket.socket(socket.AF_UNIX)
                u.connect(os.path.join(D, "ui.sock"))
            except OSError:
                c.close()
                continue
            self.conns += [c, u]
            self.hellos += 1
            threading.Thread(target=self.pump, args=(c, u, True), daemon=True).start()
            threading.Thread(target=self.pump, args=(u, c, False), daemon=True).start()

    def pump(self, a, b, up):
        try:
            while data := a.recv(65536):
                if up and self.cut_at and b'"pane.attach"' in data:
                    self.attaches_after_cut += 1
                b.sendall(data)
        except OSError:
            pass
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def cut(self, secs):
        self.blocked_until = time.time() + secs
        self.cut_at = time.time()
        for s in self.conns:
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.conns = []

shutil.rmtree(B, ignore_errors=True)
for d in (B, D, P, os.path.dirname(CFG)):
    os.makedirs(d, 0o700, exist_ok=True)
os.chmod(B, 0o700)
env = {**os.environ, "GEMBALA_RUNDIR": D, "GEMBALA_CONFIG": CFG}
r, w = os.pipe()
d = subprocess.Popen([os.path.join(app, "Contents/MacOS/gembala"), "daemon", "--secret-fd", str(w)], env=env, pass_fds=(w,), start_new_session=True)
os.close(w)
secret = os.fdopen(r).readline().strip()

def dcall(i, m, p=None):
    s = socket.socket(socket.AF_UNIX); s.connect(f"{D}/ui.sock"); f = s.makefile("rwb")
    for n, (mm, pp) in enumerate([("hello", {"proto": 1, "secret": secret}), (m, p or {})]):
        f.write((json.dumps({"id": n, "method": mm, "params": pp}) + "\n").encode()); f.flush()
        while (v := json.loads(f.readline())).get("id") != n:
            pass
    s.close()
    return v

for i in range(2):
    dcall(i, "pane.create")
panes0 = dcall(0, "pane.list")["result"]
boot0 = dcall(0, "daemon.info")["result"]["boot"]

px = Proxy()
before = pids("Gembala.app/Contents/MacOS/gembala-ui")
subprocess.run(["open", "-n", "--env", f"GEMBALA_RUNDIR={P}", "--env", f"GEMBALA_CONFIG={CFG}", "--env", f"GEMBALA_DEV_SECRET={secret}", app], check=True)
fail = 0
if not wait(lambda: os.path.exists(LAYOUT) and ids(layout()) == sorted(panes0), 20):
    print("GAGAL: layout awal tidak memuat 2 pane daemon"); fail = 1
hellos0 = px.hellos
time.sleep(2)
px.cut(3)  # only the UI's connection: the daemon is untouched
print("koneksi UI diputus, tolak sambung 3 s…")
time.sleep(14)
L = layout()
panes1 = dcall(0, "pane.list")["result"]
boot1 = dcall(0, "daemon.info")["result"]["boot"]
print("pane layout:", ids(L), "· pane daemon:", sorted(panes1), "· boot sama:", boot1 == boot0 and L["boot"] == boot0, "· sambung ulang:", px.hellos - hellos0, "· attach ulang:", px.attaches_after_cut)
if any(p < 0 for p in ids(L)) or ids(L) != sorted(panes0):
    print("GAGAL: pane ditandai mati"); fail = 1
if sorted(panes1) != sorted(panes0) or boot1 != boot0:
    print("GAGAL: daemon berubah atau ada pane baru"); fail = 1
if px.hellos - hellos0 < 1 or px.attaches_after_cut < 1:
    print("GAGAL: UI tidak sambung ulang / attach ulang"); fail = 1
for p in pids("Gembala.app/Contents/MacOS/gembala-ui") - before:
    os.kill(p, 15)
time.sleep(1)
d.kill(); d.wait()
shutil.rmtree(B, ignore_errors=True)
print("LOLOS" if not fail else "ADA YANG GAGAL")
sys.exit(fail)
