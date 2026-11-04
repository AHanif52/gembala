<p align="center">
  <img src="ui/src-tauri/icons/128x128@2x.png" width="96" alt="">
</p>

<h1 align="center">Gembala</h1>

<p align="center">
  Terminal macOS untuk ngegembalain banyak AI coding agent sekaligus.<br>
  Claude Code, Codex, OpenCode, dan Oh My Pi jalan berdampingan, dan bisa saling lempar tugas tanpa copy-paste.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/lisensi-MIT-blue" alt="Lisensi MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS-lightgrey" alt="macOS">
  <img src="https://img.shields.io/badge/status-alpha-orange" alt="Alpha">
</p>

---

Kalau lu biasa buka tiga-empat agent di tab terminal terpisah, lu pasti kenal rasanya: bolak-balik ngecek mana yang
udah kelar, mana yang nunggu izin, terus nyalin hasil agent satu ke agent lain pakai tangan. Gembala ngumpulin semua
agent itu di satu jendela, nunjukin status tiap agent, dan ngasih jalur resmi buat agent saling kirim pesan. Lu tetap
yang megang kendali.

Gembala cuma wadah terminal. Agent dijalankan dari binary resminya sendiri, apa adanya, dan ngobrol langsung ke
penyedianya. Gembala nggak jadi proxy, nggak baca token, dan nggak ngubah config agent lu.

## Fitur

**Terminal ala Warp**
- Workspace, tab, dan split pane bebas. Tata letak dan isi pane balik lagi saat app dibuka ulang.
- Kotak ketik ala editor (Shift+Enter baris baru), blok perintah per giliran, cari, palet perintah (Cmd+P).
- Tema terang/gelap, warna tab dan workspace, zoom per pane, pintasan bisa diganti semua.

**Ngurus banyak agent**
- Profil siap pakai untuk Claude Code, Codex, OpenCode, dan Oh My Pi. Agent yang lu ketik sendiri di shell juga
  dikenali.
- Status tiap agent kelihatan di tab dan sidebar: kerja, selesai, diam, atau butuh izin. Notifikasi macOS dan Dock
  cuma "lompat" saat agent butuh lu.
- Agent tetap jalan walau jendela ditutup (Cmd+Q). Buka lagi, semua masih di tempatnya.
- Lanjutkan sesi lama, jalankan agent di git worktree baru, dan beberapa akun Claude di satu mesin.
- Panel perubahan: diff git per giliran agent, plus titik kembali kalau hasilnya nggak sesuai.
- Template peran (koordinator, planner, pekerja, reviewer) untuk bagi kerja antar agent.

**Agent saling kirim pesan, diawasi**
- Ketik `@shell-2` di kotak ketik atau tekan Cmd+Shift+K buat ngelempar tugas ke agent lain lengkap dengan konteksnya.
- Dari dalam pane, agent bisa pakai CLI `gembala` (`send`, `wait`, `read`, `reply`, `status`) buat ngobrol sama
  agent lain.
- Tiap pesan dikasih label asal, dibatasi aturan akses per workspace, batas laju, dan batas hop. Pesan ke agent yang
  lagi nunggu izin ditahan. Satu tombol (Cmd+Shift+.) buat ngehentiin semua pesan.
- Laci pesan (Cmd+J) nyimpen riwayat, dan semua pesan tercatat di log audit yang ditandatangani HMAC.

## Privasi dan keamanan

- **Lokal penuh.** Nggak ada telemetri, nggak ada pembaruan otomatis, nggak ada koneksi internet dari Gembala. Daemon
  cuma buka socket Unix di folder `0700` milik akun lu, tanpa port TCP.
- **Token agent nggak disentuh.** Gembala nggak baca, nyimpen, atau neruskan token OAuth maupun API key, dan nggak
  ngubah `~/.claude`, `~/.codex`, `~/.config/opencode`, atau Keychain milik agent. Hook status dipasang lewat flag saat
  agent dijalankan.
- **Manusia yang mutusin.** Gembala nggak pernah ngejawab dialog izin agent. Aksi seperti setujui pesan, paksa kirim,
  atau ubah aturan cuma bisa dari jendela app, nggak bisa dari dalam pane.
- **Prompt injection dibatasi.** Ancaman utama sistem multi-agent adalah instruksi jahat yang nyebar dari agent ke
  agent. Label asal, aturan akses, batas laju/hop, dan tombol henti ada buat itu.

Nemu celah keamanan? Jangan buka issue publik, kirim lewat
[private vulnerability reporting](https://github.com/AHanif52/gembala/security/advisories/new).

## Kebutuhan

- macOS (diuji di Apple Silicon)
- Minimal satu agent terpasang:

  | Agent | Pasang |
  |---|---|
  | [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `brew install --cask claude-code` |
  | [Codex](https://github.com/openai/codex) | `npm install -g @openai/codex` |
  | [OpenCode](https://opencode.ai) | `brew install anomalyco/tap/opencode` |
  | Oh My Pi | `brew install can1357/tap/omp` |

## Pasang

Belum ada rilis siap unduh. Untuk sekarang build dari source (rilis lewat Homebrew menyusul).

1. Siapkan [Rust](https://rustup.rs) (`brew install rustup`) dan Node.
2. Buat sertifikat penanda tangan lokal **Gembala Dev** sekali di mesin lu: langkahnya di
   [CONTRIBUTING.md](CONTRIBUTING.md#siapkan-mesin). Daemon pakai tanda tangan ini buat ngenalin jendela app.
3. Build dan pasang ke `/Applications`:

   ```sh
   git clone https://github.com/AHanif52/gembala.git
   cd gembala/ui && npm install --ignore-scripts && cd ..
   scripts/pasang.sh
   ```

Versi lama dipindah ke Trash, bukan ditimpa. Agent yang lagi jalan nggak keganggu; versi baru kepakai setelah
Gembala ditutup lalu dibuka lagi.

## Cara pakai

Buka Gembala, pilih folder workspace, terus mulai agent dari panel Mulai atau Cmd+T. Pintasan yang paling sering
kepakai:

| Pintasan | Aksi |
|---|---|
| Cmd+T | Tab baru dari profil |
| Cmd+D / Cmd+Shift+D | Split kanan / bawah |
| Cmd+Option+panah | Pindah fokus pane |
| Cmd+P | Palet perintah |
| Cmd+Shift+K | Kirim ke agent lain |
| Cmd+Shift+U | Lompat ke pane yang butuh izin |
| Cmd+J | Laci pesan |
| Cmd+Shift+. | Hentikan semua pesan |
| Cmd+Shift+= | Panel perubahan |
| Cmd+F | Cari |
| Cmd+, | Pengaturan |

Semua pintasan bisa diganti di Pengaturan > Pintasan atau bagian `[keys]` di config.

### CLI untuk agent

`gembala` otomatis ada di PATH tiap pane, jadi agent bisa manggil langsung:

```
gembala status                                   # agent lain di workspace ini
gembala send <pane> <pesan…> [--queue] [--reply-to <msg_id>]
gembala wait [<pane>] [--reply <msg_id>] [--pattern <regex>] [--timeout <detik>]
gembala read <pane> [-n <baris>]                 # isi layar pane, rahasia disamarkan
gembala reply <msg_id> <teks…>
gembala watch                                    # event status, satu baris JSON per perubahan
```

Keluarannya satu baris JSON. Kode keluar 0 = diterima, 1 = ditolak/timeout, 2 = salah pakai.

### Data dan config

Semua data ada di `~/Library/Application Support/Gembala/` (`config.toml`, tata letak, hook, log audit). Daftar
proses yang dijalankan Gembala beserta RAM dan CPU-nya ada di Pengaturan > Proses.

## Cara kerjanya

```
Gembala.app (Tauri 2 + Svelte 5 + xterm.js)
      │  socket Unix, kanal UI (dikenali dari tanda tangan kode)
      ▼
gembala daemon (Rust) ── memegang PTY ──► zsh, claude, codex, opencode, omp
      ▲
      │  socket Unix, kanal pane
CLI gembala di dalam pane (send / wait / read / reply)
```

Daemon yang megang semua terminal, jendela app cuma klien. Makanya agent tetap hidup waktu jendela ditutup atau
app di-update. Ringan: sekitar 110–190 MB RAM dengan 4 pane, `.dmg` sekitar 3 MB.

## Status

Alpha. Fondasi, GUI, dan orkestrasi pesan udah jalan dan lagi dipakai harian oleh pembuatnya. Masih bakal ada perubahan
besar sebelum 1.0. Yang direncanakan berikutnya: rilis Homebrew yang ditandatangani dan dinotarisasi, lalu Linux.

## Berkontribusi

Kontribusi dipersilakan. Baca [CONTRIBUTING.md](CONTRIBUTING.md) untuk alur kerja dan
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) untuk struktur kode, cara uji, dan jebakan yang udah ketemu. Fitur baru:
buka issue dulu biar dibahas sebelum dikerjain.

## Lisensi

[MIT](LICENSE)
