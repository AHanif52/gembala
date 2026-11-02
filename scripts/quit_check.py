# FR-20 dengan app debug asli: UI menjalankan daemon sendiri, pane sibuk (sleep), lalu UI ditutup seperti Cmd+Q
# (NSRunningApplication.terminate ke PID instance uji saja). Lolos = daemon dan sleep di pane tetap hidup.
# Folder run, config, dan shell terpisah; instance Gembala lain tidak tersentuh.
# Argumen kedua `stop`: pengaturan "Saat Cmd+Q: Hentikan semua"; lolos = daemon dan sleep ikut berhenti.
# pakai: npm run tauri build -- --debug && python3 -I scripts/quit_check.py target/debug/bundle/macos/Gembala.app [stop]
import os, shutil, subprocess, sys, time

app = sys.argv[1]
R = os.path.join(os.environ["TMPDIR"], "gquit")
shutil.rmtree(R, ignore_errors=True)
os.makedirs(os.path.join(R, "cfg"))
os.chmod(R, 0o700)
stop = sys.argv[2:] == ["stop"]
if stop:
    open(os.path.join(R, "cfg", "config.toml"), "w").write('[ui]\non_quit = "stop"\n')
# shell palsu: perintah `-c` (deteksi agent) diteruskan ke zsh, shell pane interaktif menjalankan sleep
sh = os.path.join(R, "fakesh")
open(sh, "w").write('#!/bin/sh\ncase "$1$2" in *-c*) exec /bin/zsh "$@";; esac\nsleep 600 &\nwait\n')
os.chmod(sh, 0o755)

def pids(*args):
    return {int(p) for p in subprocess.run(["pgrep", *args], capture_output=True, text=True).stdout.split()}

def daemon():
    out = subprocess.run(["lsof", "-t", f"{R}/ui.sock"], capture_output=True, text=True).stdout.split()
    return {int(p) for p in out} - pids("-f", "MacOS/gembala-ui")

def wait(cond, secs):
    end = time.time() + secs
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.3)
    return False

def alive(p):
    return subprocess.run(["ps", "-p", str(p)], capture_output=True).returncode == 0

before = pids("-f", "MacOS/gembala-ui")
subprocess.run(["open", "-n", "--env", f"GEMBALA_RUNDIR={R}", "--env", f"GEMBALA_CONFIG={R}/cfg/config.toml",
                "--env", f"SHELL={sh}", app], check=True)
fail = 0
wait(lambda: pids("-f", "MacOS/gembala-ui") - before, 15)
ui = (pids("-f", "MacOS/gembala-ui") - before).pop()
sleeps = lambda d: pids("-x", "sleep") & {c for s in pids("-P", str(d)) for c in pids("-P", str(s))}
if not wait(daemon, 15) or not wait(lambda: sleeps(daemon().pop()), 15):
    print("GAGAL: daemon atau pane sleep tidak muncul"); fail = 1
else:
    d = daemon().pop()
    sl = sleeps(d).pop()
    time.sleep(2)
    subprocess.run(["osascript", "-l", "JavaScript", "-e",
                    f"ObjC.import('AppKit'); $.NSRunningApplication.runningApplicationWithProcessIdentifier({ui}).terminate"],
                   check=True, capture_output=True)
    time.sleep(5)  # loginwindow menghentikan proses coalition ±1 detik setelah app keluar
    print("ui:", "hidup" if alive(ui) else "mati", "· daemon:", "hidup" if alive(d) else "mati", "· sleep di pane:", "hidup" if alive(sl) else "mati")
    if alive(ui) or alive(d) != (not stop) or alive(sl) != (not stop):
        print("GAGAL:", "daemon atau pane masih hidup" if stop else "daemon atau pane ikut berhenti saat app ditutup"); fail = 1
for p in pids("-f", "MacOS/gembala-ui") - before:
    os.kill(p, 15)
for p in daemon():
    os.kill(p, 9)
subprocess.run(["pkill", "-f", sh])
shutil.rmtree(R, ignore_errors=True)
print("LOLOS" if not fail else "ADA YANG GAGAL")
sys.exit(fail)
