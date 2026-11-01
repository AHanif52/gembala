# Uji FR-45 dengan app debug asli: tata letak bertahan saat UI dibuka ulang, dan pane dari daemon lama
# langsung diganti shell baru saat daemon dijalankan ulang. Folder run dan config terpisah dari milik pengguna.
# pakai: npm run tauri build -- --debug && python3 -I scripts/restore_check.py target/debug/bundle/macos/Gembala.app
import json, os, shutil, socket, subprocess, sys, time

app = sys.argv[1]
daemon_bin = os.path.join(app, "Contents/MacOS/gembala")
R = os.path.join(os.environ["TMPDIR"], "grestore")
env = {**os.environ, "GEMBALA_RUNDIR": R, "GEMBALA_CONFIG": os.path.join(R, "cfg", "config.toml")}
LAYOUT = os.path.join(R, "cfg", "layout.json")

def pids(pattern):
    out = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True).stdout
    return {int(p) for p in out.split()}

def start_daemon(n):
    r, w = os.pipe()
    d = subprocess.Popen([daemon_bin, "daemon", "--secret-fd", str(w)], env=env, pass_fds=(w,), start_new_session=True)
    os.close(w)
    secret = os.fdopen(r).readline().strip()
    s = socket.socket(socket.AF_UNIX)
    s.connect(f"{R}/ui.sock")
    f = s.makefile("rwb")
    def call(i, m, p):
        f.write((json.dumps({"id": i, "method": m, "params": p}) + "\n").encode()); f.flush()
        while True:
            v = json.loads(f.readline())
            if v.get("id") == i: return v
    call(0, "hello", {"proto": 1, "secret": secret})
    for i in range(n):
        call(i + 1, "pane.create", {})
    s.close()
    return d, secret

def ui_session(secret, wait=8):
    before = pids("Gembala.app/Contents/MacOS/ui")
    mtime = os.path.getmtime(LAYOUT) if os.path.exists(LAYOUT) else 0
    subprocess.run(["open", "-n", "--env", f"GEMBALA_RUNDIR={R}", "--env", f"GEMBALA_DEV_SECRET={secret}", app], check=True)
    t0 = time.time()
    while time.time() - t0 < wait and not (os.path.exists(LAYOUT) and os.path.getmtime(LAYOUT) > mtime):
        time.sleep(0.2)
    time.sleep(1.5)
    for p in pids("Gembala.app/Contents/MacOS/ui") - before:
        os.kill(p, 15)
    time.sleep(1)
    with open(LAYOUT) as fh:
        return json.load(fh)

def ids(L):
    def walk(n):
        return [n["pane"]] if "pane" in n else walk(n["a"]) + walk(n["b"])
    return sorted(p for w in L["workspaces"] for t in w["tabs"] for p in walk(t["tree"]))

shutil.rmtree(R, ignore_errors=True)
os.mkdir(R, 0o700)
fail = 0
d, secret = start_daemon(2)
first = ui_session(secret)
second = ui_session(secret)
print("buka pertama:", ids(first), "· dibuka ulang:", ids(second))
if ids(first) != ids(second) or len(ids(first)) != 2:
    print("GAGAL: tata letak berubah saat UI dibuka ulang"); fail = 1
d.kill(); d.wait()
time.sleep(1.5)
d2, secret2 = start_daemon(0)
third = ui_session(secret2)
print("setelah daemon baru:", ids(third), "boot sama:", third["boot"] == second["boot"])
if len(ids(third)) != 2 or any(p < 0 for p in ids(third)) or third["boot"] == second["boot"]:
    print("GAGAL: pane daemon lama harus langsung diganti shell baru (id positif, jumlah sama)"); fail = 1
d2.kill()
shutil.rmtree(R, ignore_errors=True)
print("LOLOS" if not fail else "ADA YANG GAGAL")
sys.exit(fail)
