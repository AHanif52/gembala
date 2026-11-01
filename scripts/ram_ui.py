# Eksperimen RAM UI (TECH M1): app kedua (build DEBUG) dibuka ke folder run uji berisi N pane
# berisi sama, lalu footprint semua proses app diukur. Rahasia lewat GEMBALA_DEV_SECRET (hanya ada
# di build debug), jadi Keychain tidak disentuh. Tidak mengganggu app/daemon yang sedang dipakai.
# pakai: npm run tauri build -- --debug && python3 -I scripts/ram_ui.py target/debug/bundle/macos/Gembala.app <n_pane>
import json, os, re, shutil, socket, subprocess, sys, time

app, n = sys.argv[1], int(sys.argv[2])
daemon_bin = os.path.join(app, "Contents/MacOS/gembala")
R = os.path.join(os.environ["TMPDIR"], f"gram-{n}")
FILL = r"""for i in $(seq 1 300); do printf '\033[3%dm%03d %s\033[0m\n' $((i%7+1)) $i "$(printf 'isi-pane-%.0s' 1 2 3 4 5 6 7 8 9 10)"; done"""

def pids(pattern):
    out = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True).stdout
    return {int(p) for p in out.split()}

def footprint(pids_):
    args = ["footprint"] + [x for p in sorted(pids_) for x in ("-p", str(p))]
    out = subprocess.run(args, capture_output=True, text=True).stdout
    rows = re.findall(r"^(\S+) \[(\d+)\]: 64-bit\s+Footprint: ([\d.]+) (KB|MB)", out, re.M)
    return {name: float(v) / (1024 if unit == "KB" else 1) for name, _, v, unit in rows}

shutil.rmtree(R, ignore_errors=True)
os.mkdir(R, 0o700)
r, w = os.pipe()
d = subprocess.Popen([daemon_bin, "daemon", "--secret-fd", str(w)], env={**os.environ, "GEMBALA_RUNDIR": R, "GEMBALA_CONFIG": os.path.join(R, "cfg", "config.toml")},
                     pass_fds=(w,), start_new_session=True)
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
    call(i + 1, "pane.create", {"cmd": FILL + "; exec $SHELL -l", "rows": 40, "cols": 100})
s.close()

before_ui = pids("Gembala.app/Contents/MacOS/gembala-ui")
before_wk = pids("com.apple.WebKit")
subprocess.run(["open", "-n", "--env", f"GEMBALA_RUNDIR={R}", "--env", f"GEMBALA_DEV_SECRET={secret}", app], check=True)
series, last = [], {}
t0 = time.time()
time.sleep(1.5)
# jendela tertutup jendela lain = WebKit berhenti menggambar; paksa ke depan supaya angka sebanding
for p in pids("Gembala.app/Contents/MacOS/gembala-ui") - before_ui:
    subprocess.run(["osascript", "-e", f'tell application "System Events" to set frontmost of (first process whose unix id is {p}) to true'], capture_output=True)
for at in (5, 10, 20, 30):
    time.sleep(max(0, at - (time.time() - t0)))
    ui = pids("Gembala.app/Contents/MacOS/gembala-ui") - before_ui
    wk = pids("com.apple.WebKit") - before_wk
    last = footprint(ui | wk | {d.pid})
    series.append(sum(last.values()))
# daemon: 1 listener ui.sock + 1 koneksi per klien UI; tanpa koneksi = UI gagal sambung, angka tidak sah
conns = subprocess.run(["lsof", "-a", "-U", "-p", str(d.pid)], capture_output=True, text=True).stdout.count("ui.sock") - 1
parts = ", ".join(f"{k.replace('com.apple.WebKit.', '')} {v:.0f}" for k, v in sorted(last.items(), key=lambda x: -x[1]))
ok = "ok" if conns > 0 else "UI TIDAK tersambung"
lay = os.path.join(R, "cfg", "layout.json")
if os.path.exists(lay):
    with open(lay) as fh:
        L = json.load(fh)
    ok += f", layout {len(L.get('workspaces', []))} ws / {sum(len(w['tabs']) for w in L['workspaces'])} tab"
print(f"{n} pane [{ok}] detik 5/10/20/30: {' / '.join(f'{x:.0f}' for x in series)} MB  akhir: {parts}")

for p in ui:
    os.kill(p, 15)
d.kill()
time.sleep(1)
shutil.rmtree(R, ignore_errors=True)
