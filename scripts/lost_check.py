# Kriteria 17 dengan app debug asli: daemon dimatikan paksa → app menjalankan daemon baru dan pane lama
# tampil sebagai pane mati (id negatif). Folder run dan config terpisah; Keychain tidak disentuh (build debug).
# pakai: npm run tauri build -- --debug && python3 -I scripts/lost_check.py target/debug/bundle/macos/Gembala.app
import json, os, shutil, socket, subprocess, sys, time

app = sys.argv[1]
R = os.path.join(os.environ["TMPDIR"], "glost")
CFG = os.path.join(R, "cfg", "config.toml")
LAYOUT = os.path.join(R, "cfg", "layout.json")
env = {**os.environ, "GEMBALA_RUNDIR": R, "GEMBALA_CONFIG": CFG}

def pids(pattern):
    out = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True).stdout
    return {int(p) for p in out.split()}

def daemon_of_rundir():
    out = subprocess.run(["lsof", "-t", f"{R}/ui.sock"], capture_output=True, text=True).stdout.split()
    return {int(p) for p in out} - pids("Gembala.app/Contents/MacOS/ui")

def layout():
    with open(LAYOUT) as fh:
        return json.load(fh)

def ids(L):
    def walk(n):
        return [n["pane"]] if "pane" in n else walk(n["a"]) + walk(n["b"])
    return sorted(p for w in L["workspaces"] for t in w["tabs"] for p in walk(t["tree"]))

def wait(cond, secs):
    t0 = time.time()
    while time.time() - t0 < secs:
        if cond():
            return True
        time.sleep(0.2)
    return False

shutil.rmtree(R, ignore_errors=True)
os.mkdir(R, 0o700)
r, w = os.pipe()
d = subprocess.Popen([os.path.join(app, "Contents/MacOS/gembala"), "daemon", "--secret-fd", str(w)], env=env, pass_fds=(w,), start_new_session=True)
os.close(w)
secret = os.fdopen(r).readline().strip()
s = socket.socket(socket.AF_UNIX); s.connect(f"{R}/ui.sock"); f = s.makefile("rwb")
for i, m in enumerate(["hello", "pane.create", "pane.create"]):
    f.write((json.dumps({"id": i, "method": m, "params": {"proto": 1, "secret": secret} if m == "hello" else {}}) + "\n").encode()); f.flush()
    f.readline()
s.close()

before = pids("Gembala.app/Contents/MacOS/ui")
subprocess.run(["open", "-n", "--env", f"GEMBALA_RUNDIR={R}", "--env", f"GEMBALA_CONFIG={CFG}", "--env", f"GEMBALA_DEV_SECRET={secret}", app], check=True)
fail = 0
if not wait(lambda: os.path.exists(LAYOUT) and len(ids(layout())) == 2, 15):
    print("GAGAL: layout awal tidak tersimpan"); fail = 1
old_boot = layout()["boot"]
d.kill(); d.wait()
ok = wait(lambda: daemon_of_rundir() and layout()["boot"] != old_boot and all(p < 0 for p in ids(layout())), 20)
L = layout()
print("daemon baru:", sorted(daemon_of_rundir()), "· pane:", ids(L), "· boot berubah:", L["boot"] != old_boot)
if not ok:
    print("GAGAL: daemon tidak dijalankan lagi atau pane lama tidak jadi pane mati"); fail = 1
for p in pids("Gembala.app/Contents/MacOS/ui") - before:
    os.kill(p, 15)
time.sleep(1)
for p in daemon_of_rundir():
    os.kill(p, 9)
shutil.rmtree(R, ignore_errors=True)
print("LOLOS" if not fail else "ADA YANG GAGAL")
sys.exit(fail)
