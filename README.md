# Gembala (app)

Terminal GUI ala Warp untuk banyak TUI agent (Claude Code, Codex, OpenCode, Oh My Pi) dalam satu layar, dengan
pesan antar agent yang diawasi. Kebutuhan: repo gembala-brd (`BRD.md` = apa/kenapa, sumber utama; `TECH.md` = cara,
catatan per milestone, dan tabel "Sisa Fase 1").

## Status (1 Nov 2026)

| Fase | Status |
| --- | --- |
| 0 Fondasi | Selesai. T1–T8 diputus (TECH bagian 14) |
| 1 GUI (M1–M7) | Kode selesai, semua uji otomatis lolos. Menunggu uji langsung + kriteria 2, 15, 16 (pemakaian nyata) |
| 2 Orkestrasi (O1–O4) | Kode selesai. Kriteria 1, 3–11, 17 punya uji otomatis yang lolos |
| 3 Lanjutan | Belum. Menurut BRD bagian 16 ditentukan setelah Fase 2 dipakai sendiri 2 minggu |

Keputusan yang masih menunggu pengguna: pintasan Fase 2 (`⌘⇧K` kirim, `⌘J` laci, `⌘⇧.` henti; usulan Claude),
sertifikat tanda tangan "Gembala Dev", hapus trailer Co-Authored-By di commit lama (perlu force-push).

## Struktur

```
core/                    binary `gembala`: daemon + CLI (satu binary, juga jadi sidecar app)
  src/main.rs            subcommand: daemon, hook, send, wait, read, reply, status, watch
  src/daemon.rs          socket ui.sock + pane.sock, registri pane, attach, status, mesin pesan (glue)
  src/pty.rs             spawn login shell (setsid, env bersih SEC-35), busy/nama/cwd/argumen proses
  src/grid.rs            grid VT (vt100) + snapshot attach + teks untuk read/pola layar
  src/config.rs          config.toml (toml_edit): validasi per kunci, set/profil/pintasan, tulis atomik
  src/profile.rs         profil agent, deteksi, perintah launch + flag hook + sesi, status hook/pola layar
  src/msg.rs             aturan pesan murni: penyaring, label, keputusan kirim (BRD bagian 8)
  src/bus.rs             antrean, riwayat, log audit HMAC (SEC-11)
  src/git.rs             git status/diff baca saja + redaksi rahasia (SEC-12, 29)
  src/rundir.rs          folder socket privat (SEC-1, 25)
  tests/daemon.rs        uji integrasi daemon nyata lewat socket (termasuk kriteria BRD)
ui/                      Tauri 2 + SvelteKit static + Svelte 5 + xterm.js (renderer DOM)
  src-tauri/src/lib.rs   klien daemon (tidak membuka PTY, R-5), menu native + [keys], notifikasi, dialog, paste
  src-tauri/src/daemon.rs  sambung/jalankan daemon, rahasia kanal UI di Keychain
  src/routes/+page.svelte  workspace → tab → pohon split, sidebar, toolbar, kartu, pemulihan
  src/lib/               Tile (satu xterm per pane), Settings, Changes, Messages, OutputView, layout (murni)
scripts/                 alat uji app asli di folder run + config terpisah (tidak menyentuh data pengguna)
```

Data pengguna: `~/Library/Application Support/Gembala/` (config.toml, layout.json, hooks/, audit/), socket di
`$TMPDIR/gembala/`. Bisa dialihkan dengan `GEMBALA_CONFIG` dan `GEMBALA_RUNDIR` (dipakai uji).

## Jalankan dan uji

Butuh Rust (`brew install rustup`; PATH `/opt/homebrew/opt/rustup/bin`) dan Node. JDK tidak perlu.

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
python3 -I scripts/restore_check.py target/debug/bundle/macos/Gembala.app   # FR-45: buka ulang + daemon baru
python3 -I scripts/lost_check.py target/debug/bundle/macos/Gembala.app      # kriteria 17: daemon dibunuh
python3 -I scripts/ram_ui.py target/debug/bundle/macos/Gembala.app 4        # RAM 4 pane (perlu layar menyala)
python3 -I scripts/ukur.py target/release/gembala $TMPDIR/gbl-ukur          # latensi + throughput daemon
```

Instal npm selalu dengan `--ignore-scripts` (SEC-16).

## Pintasan bawaan

`⌘T` tab baru dari profil · `⌘D`/`⌘⇧D` split · `⌘⌥←→↑↓` fokus · `⌃⌘←→↑↓` geser pemisah · `⌘⇧↩` maksimalkan ·
`⌘.` hentikan proses · `⌘W` tutup pane · `⌘1–9` tab · `⌘P` cari tab · `⌘B` sidebar · `⌘F` cari · `⌘K` bersihkan ·
`⌘=`/`⌘-`/`⌘0` zoom · `⌥⌘=`… zoom pane · `⌘G` Rich Input · `⌘⇧G` panel perubahan · `⌘,` Pengaturan ·
`⌘⇧K` kirim ke agent · `⌘J` laci pesan · `⌘⇧.` henti semua pesan. Ganti di Pengaturan > Pintasan atau `[keys]`.

## CLI untuk agent (di dalam pane)

```
gembala status                                   # agent lain di workspace ini
gembala send <pane> <pesan…> [--queue] [--reply-to <msg_id>]
gembala wait [<pane>] [--reply <msg_id>] [--pattern <regex>] [--timeout <detik>]
gembala read <pane> [-n <baris>]                 # teks polos tersamar, dibungkus pembatas
gembala reply <msg_id> <teks…>
gembala watch                                    # event status, satu baris JSON per perubahan
```

Keluaran satu baris JSON; kode keluar 0 diterima, 1 ditolak/timeout, 2 salah pakai.

## Jebakan yang sudah ketemu

- WKWebView tidak meneruskan banyak kombinasi ⌘ ke keydown JS: semua pintasan lewat menu native; `⌘⇧↩` justru
  dimakan WebKit sebelum menu, jadi ditangkap keydown (dengan penjaga 150 ms).
- Build tanpa tanda tangan tetap = dialog Keychain "ui" sekali tiap build baru (tanpa Always Allow). Item Keychain
  dari CLI `security -A` tetap ditanya (partition list). Alat uji pakai build debug + `GEMBALA_DEV_SECRET`.
- App dibuka dari dalam sesi agent mewarisi env sesi itu: dibuang di `pty::scrub_env` (SEC-35).
- Lock `Mutex` std tidak reentrant: jangan memegang `panes` sambil memanggil fungsi yang menguncinya lagi.
- RAM: renderer DOM xterm lebih hemat dan datar per pane daripada WebGL (TECH catatan M1).

## Cara kerja di repo ini

- Urutan: BRD → TECH → kode/mockup.
- Tiap pekerjaan selesai dan uji hijau: commit (path spesifik, tanpa trailer Co-Authored-By karena remote GitHub),
  push lewat OMP: `omp -p --no-session "Jalankan PERSIS … git -C app push origin main …" < /dev/null`
  (tanpa `< /dev/null` omp menunggu stdin).
