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

Diputus 1 Nov 2026: pintasan Fase 2 tetap (`⌘⇧K` kirim, `⌘J` laci, `⌘⇧.` henti); app ditandatangani sertifikat
self-signed "Gembala Dev" di keychain login; trailer Co-Authored-By dihapus dari riwayat (force-push; cadangan
lokal di branch `cadangan/sebelum-hapus-trailer`, tidak di-push).

**Uji langsung 1 Nov 2026 (pengguna, app rilis):** jalan: Shift+Enter baris baru, × tutup tab, konfirmasi ⌘W,
kirim ⌘⇧K, split, panel perubahan, ⌘F, dialog folder workspace, blok ringkas saat agent keluar dengan id sesi
(Claude, OMP; Codex ditambahkan setelahnya), pesan antar agent dua arah (`send` + `reply`). Diperbaiki di sesi itu:
`not_a_pane` dari tool Bash Claude (SEC-3 rantai induk), ⌘K merusak TUI, autokoreksi textarea, pane mati saat app
dibuka (FR-45 jadi shell baru), agent manual tak terlihat di `status`, `gembala` belum di PATH pane, tutup jendela
tidak keluar dari app. Pengguna menilai UI/UX belum cukup untuk pemakaian 2 minggu: backlog di TECH bagian 17.

**UI/UX (TECH 17, 1 Nov 2026):** 14 butir dikerjakan: palet `⌘P`, menu klik kanan tab/workspace/terminal, seret tab
dan workspace, workspace kosong, status bertulisan, warna tab, lebar sidebar, kepala pane, kartu agent selesai, huruf
ikon Nerd Font, pemilih tujuan kirim. Uji otomatis lolos; menu native dan seret belum diuji tangan.

**Menunggu:** (1) uji langsung UI/UX di atas; (2) id pesan dari pengguna dibuang dari label supaya agent
tidak mencoba `reply` (opsi a, dikerjakan sesi lain di pane, cek BRD/daemon sebelum lanjut); (3) agent belum tahu
perintah `gembala`: usulan paragraf petunjuk di `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, dan setara OMP
(ditulis atas izin pengguna; Gembala sendiri tidak boleh, SEC-15); (4) ukur RAM dengan agent jalan (`footprint`).

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
`⌘.` hentikan proses · `⌘W` tutup pane · `⌘1–9` tab · `⌘P` palet perintah · `⌘B` sidebar · `⌘F` cari · `⌘K` bersihkan ·
`⌘=`/`⌘-`/`⌘0` zoom · `⌥⌘=`… zoom pane · `⌘G` Rich Input · `⌘⇧G` panel perubahan · `⌘,` Pengaturan ·
`⌘⇧K` kirim ke agent · `⌘J` laci pesan · `⌘⇧.` henti semua pesan. Ganti di Pengaturan > Pintasan atau `[keys]`.

## CLI untuk agent (di dalam pane)

`gembala` ada di PATH tiap pane (folder binary daemon). Agent yang diketik sendiri di pane shell (claude, codex,
opencode, omp; dikenali dari nama proses) ikut terlihat di `status` dan bisa dikirimi. Nama pane-nya tetap
`shell-<n>`; label tujuan di ⌘⇧K menampilkan nama agentnya.

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
- Build rilis ditandatangani "Gembala Dev" (`bundle.macOS.signingIdentity`, sertifikat self-signed di keychain login,
  tidak perlu dipercaya untuk codeSign): designated requirement = identifier + hash sertifikat, sama tiap build, jadi
  "Always Allow" di dialog Keychain "ui" bertahan. Mesin tanpa sertifikat itu: build gagal tanda tangan; buat dengan
  `openssl req -x509` (extendedKeyUsage=codeSigning) lalu `security import`. Item Keychain dari CLI `security -A`
  tetap ditanya (partition list). Alat uji pakai build debug + `GEMBALA_DEV_SECRET`.
- App dibuka dari dalam sesi agent mewarisi env sesi itu: dibuang di `pty::scrub_env` (SEC-35).
- Lock `Mutex` std tidak reentrant: jangan memegang `panes` sambil memanggil fungsi yang menguncinya lagi.
- RAM: renderer DOM xterm lebih hemat dan datar per pane daripada WebGL (TECH catatan M1).
- Daemon tetap hidup setelah UI ditutup. UI build baru yang tersambung ke daemon build lama menampilkan
  `gagal sambung ke daemon: unknown_method`. Tiap rebuild release: matikan daemon lama
  (`pkill -f 'Gembala.app/Contents/MacOS/gembala daemon'`, pane yang hidup ikut mati), ganti
  `/Applications/Gembala.app` dengan `target/release/bundle/macos/Gembala.app`, buka dari `/Applications` saja.
  Bila hanya UI yang berubah (core dan PROTO sama), cukup matikan proses `ui`; pane tetap hidup, kecuali semua pane
  diam: daemon tanpa UI berhenti sendiri 5–10 detik kemudian (FR-20). UI yang tersambung tepat saat daemon berhenti
  dapat "Broken pipe" di tengah hello; sekarang UI menunggu 0,5 detik lalu sambung/jalankan daemon lagi sekali.
  Pasang dari folder mana pun: `G=…/app/target/release/bundle/macos/Gembala.app; [ -d "$G" ] && rm -rf
  /Applications/Gembala.app && cp -R "$G" /Applications/` (cek dulu supaya app lama tidak terhapus bila build gagal).
- Tool Bash Claude Code membuat sesi proses baru (setsid): identitas pane harus lewat rantai proses induk, bukan
  `getsid` peer saja (SEC-3, `pane_of_pid`).
- Deteksi agent manual memakai nama file proses (`proc_pidpath`); agent yang dibungkus interpreter (nama proses
  `node`/`bun`) tidak terdeteksi. Claude (Homebrew Cask), Codex 0.166, dan OMP terdeteksi.
- Salinan `/bin/sleep` dibunuh macOS (tanda tangan platform): agent palsu di uji = salinan biner `gembala` bernama
  `omp` menjalankan `watch`.
- Sebelum commit kode Rust: `cargo clippy --workspace --all-targets -- -D warnings` (TECH bagian 16), bukan cuma
  `cargo test`.
- Push lewat OMP kadang ditolak OMP ("tunjukkan diff dulu"): sertakan daftar commit + diff stat di prompt.
- Codex yang baru di-update menolak hook plugin pihak ketiga ("plugin contents changed; hook approval required"):
  bukan dari Gembala; setujui ulang hook di Codex.

## Cara kerja di repo ini

- Urutan: BRD → TECH → kode/mockup.
- Tiap pekerjaan selesai dan uji hijau: commit (path spesifik, tanpa trailer Co-Authored-By karena remote GitHub),
  push lewat OMP: `omp -p --no-session "Jalankan PERSIS … git -C app push origin main …" < /dev/null`
  (tanpa `< /dev/null` omp menunggu stdin).
