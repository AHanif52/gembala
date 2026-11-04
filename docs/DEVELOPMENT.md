# Catatan pengembangan

Nomor seperti FR-61, SEC-20, atau "TECH bagian 17" di kode merujuk ke dokumen kebutuhan pemilik repo yang tidak
dibuka; konteks yang lu perlu ada di issue. Alur kerja dan aturan PR ada di [CONTRIBUTING.md](../CONTRIBUTING.md).

## Struktur

```
core/                    binary `gembala`: daemon + CLI (satu binary, juga jadi sidecar app)
  src/main.rs            subcommand: daemon, hook, send, wait, read, reply, status, watch
  src/daemon.rs          socket ui.sock + pane.sock, registri pane, attach, status, mesin pesan (glue)
  src/pty.rs             spawn login shell (setsid, env bersih SEC-35), busy/nama/cwd/argumen proses
  src/grid.rs            grid VT (vt100) + snapshot attach + teks untuk read/pola layar
  src/config.rs          config.toml (toml_edit): validasi per kunci, set/profil/pintasan, tulis atomik
  src/profile.rs         profil agent, deteksi, perintah launch + flag hook + sesi, status hook/pola layar
  src/msg.rs             aturan pesan murni: penyaring, label, keputusan kirim
  src/bus.rs             antrean, riwayat, log audit HMAC (SEC-11)
  src/git.rs             git status/diff baca saja + redaksi rahasia (SEC-12, 29)
  src/rundir.rs          folder socket privat (SEC-1, 25)
  src/sign.rs            kanal UI dikenali dari tanda tangan kode peer (SEC-20)
  tests/daemon.rs        uji integrasi daemon nyata lewat socket
ui/                      Tauri 2 + SvelteKit static + Svelte 5 + xterm.js (renderer DOM)
  src-tauri/src/lib.rs   klien daemon (tidak membuka PTY), menu native + [keys], notifikasi, dialog, paste
  src-tauri/src/daemon.rs  sambung/jalankan daemon
  src/routes/+page.svelte  workspace → tab → pohon split, sidebar, toolbar, kartu, pemulihan
  src/lib/               Tile (satu xterm per pane), Settings, Changes, Messages, OutputView, layout (murni)
scripts/                 pasang.sh + alat uji app asli di folder run + config terpisah (tidak menyentuh data pengguna)
```

Data pengguna: `~/Library/Application Support/Gembala/` (config.toml, layout.json, hooks/, audit/), socket di
`$TMPDIR/gembala/`. Bisa dialihkan dengan `GEMBALA_CONFIG` dan `GEMBALA_RUNDIR` (dipakai uji).

Method baru dari UI ke daemon wajib didaftarkan di `ALLOWED` (`ui/src-tauri/src/lib.rs`); kalau tidak, panggilannya
ditolak diam-diam.

## Jalankan dan uji

Butuh Rust (`brew install rustup`; PATH `/opt/homebrew/opt/rustup/bin`) dan Node.

```
export PATH=/opt/homebrew/opt/rustup/bin:$PATH
cargo test -p gembala                           # unit + integrasi daemon (~30 dtk)
cargo test -p ui                                # sisi Rust app
cargo clippy --workspace --all-targets -- -D warnings
cd ui && npm install --ignore-scripts && npm run check && npm test
npm run tauri build                             # app rilis: target/release/bundle/macos/Gembala.app (+ .dmg)
npm run tauri build -- --debug                  # app debug, untuk scripts/ di bawah
```

Alat uji dengan app debug asli (rahasia lewat `GEMBALA_DEV_SECRET`, jalur itu hanya ada di build debug):

```
python3 -I scripts/restore_check.py target/debug/bundle/macos/Gembala.app   # buka ulang + daemon baru
python3 -I scripts/lost_check.py target/debug/bundle/macos/Gembala.app      # daemon dibunuh
python3 -I scripts/quit_check.py target/debug/bundle/macos/Gembala.app      # Cmd+Q, agent tetap hidup
python3 -I scripts/ram_ui.py target/debug/bundle/macos/Gembala.app 4        # RAM 4 pane (perlu layar menyala)
python3 -I scripts/ukur.py target/release/gembala $TMPDIR/gbl-ukur          # latensi + throughput daemon
```

Instal npm selalu dengan `--ignore-scripts` (SEC-16).

## Proses yang dijalankan Gembala

| Nama di Activity Monitor | Peran |
|---|---|
| `gembala-ui` (atau "Gembala") | Jendela aplikasi |
| `tauri://localhost` | Tampilan WebKit: terminal dan sidebar |
| WebKit GPU dan Networking | Bagian bawaan WebKit untuk menggambar dan memuat tampilan dari dalam app |
| `gembala` | Daemon penjaga pane: shell dan agent berjalan di bawahnya |
| `zsh`, `claude`, `codex`, … | Proses yang dijalankan di pane |

- CSP di `ui/src-tauri/tauri.conf.json` hanya mengizinkan kanal internal app; daemon hanya membuka socket Unix di
  folder `0700`.
- Daemon tetap hidup setelah jendela ditutup hanya bila ada pane yang masih menjalankan proses (FR-20); notifikasi
  macOS menyebut alasannya. Tanpa itu, daemon berhenti sendiri dalam beberapa detik.

## Jebakan yang sudah ketemu

- WKWebView tidak meneruskan banyak kombinasi Cmd ke keydown JS: semua pintasan lewat menu native; Cmd+Shift+Enter
  justru dimakan WebKit sebelum menu, jadi ditangkap keydown (dengan penjaga 150 ms).
- Build rilis ditandatangani "Gembala Dev" (`bundle.macOS.signingIdentity`, sertifikat self-signed di keychain login):
  designated requirement = identifier + hash sertifikat, sama tiap build. Item Keychain app tanpa TeamIdentifier
  terikat ke cdhash build, jadi "Always Allow" tidak bertahan antar build. Karena itu kanal UI dikenali daemon dari
  tanda tangan peer (`core/src/sign.rs`). Alat uji pakai build debug + `GEMBALA_DEV_SECRET`.
- App yang dibuka dari dalam sesi agent mewarisi env sesi itu: dibuang di `pty::scrub_env` (SEC-35).
- Lock `Mutex` std tidak reentrant: jangan memegang `panes` sambil memanggil fungsi yang menguncinya lagi.
- RAM: renderer DOM xterm lebih hemat dan datar per pane daripada WebGL.
- Daemon tetap hidup setelah UI ditutup. UI build baru yang tersambung ke daemon build lama menampilkan
  `gagal sambung ke daemon: unknown_method`: tutup Gembala lalu buka lagi supaya daemon baru yang jalan
  (pane yang hidup ikut mati). `scripts/pasang.sh` memindah bundle lama ke Trash, jadi daemon lama tetap bisa jalan
  sampai app dibuka ulang.
- Tool Bash Claude Code membuat sesi proses baru (setsid): identitas pane harus lewat rantai proses induk, bukan
  `getsid` peer saja (SEC-3, `pane_of_pid`).
- Deteksi agent manual memakai nama file proses (`proc_pidpath`); agent yang dibungkus interpreter (nama proses
  `node`/`bun`) tidak terdeteksi.
- Salinan `/bin/sleep` dibunuh macOS (tanda tangan platform): agent palsu di uji = salinan biner `gembala` bernama
  `omp` menjalankan `watch`.
- Codex yang baru di-update menolak hook plugin pihak ketiga ("plugin contents changed; hook approval required"):
  bukan dari Gembala; setujui ulang hook di Codex.
