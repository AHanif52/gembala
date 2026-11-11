# Changelog

`scripts/rilis.sh` puts the section of the version being released on its GitHub release page.

## 0.1.9

### English

- Agents can request a model and effort when asking you to open a new pane. Choose a listed model or enter an ID in the confirmation card; a setting doesn't change an agent pane that's already open.
- Notifications say who needs you and why: the pane and its agent, what it waits for (a permission, an answer, a password, your OK, or a failure) or that it finished, and what to do next. One banner per pane; a banner goes away once you answer, and panes that finish together share one. Each kind has its own switch in Settings › General › Notifications.
- Report a problem (⌘P › Report a problem): Gembala fills in the versions, you check the text, and it opens a new GitHub issue in your browser. Nothing is sent until you press Submit there. Agents can draft one with `gembala report`.
- How much allowance is left per agent login (Claude, Codex, Oh My Pi, Gemini) shows in a rail beside the panes.
- Helper panes an agent opens show as sub-tabs under its tab, with their stage and status.
- The pane bar shrinks with its width; what doesn't fit moves into a "…" menu.
- Lighter: the default scrollback is now 2,000 lines (each line costs memory in every live pane). Already set to more? Settings › Appearance.
- Telegram: permission cards say what the agent asks for and why, with buttons right under the text (Oh My Pi, OpenCode, Claude, Codex). Codex's done card waits for its answer instead of showing its screen. Claude's web-fetch dialog now counts as "needs you".
- Telegram: a "done" card now carries the agent's own answer for Claude, Codex and Oh My Pi (the pane's screen only when the agent has no answer hook). Claude's tool-call rows stay off the phone, and a screen showing a picker or dialog (Claude's `/model`, a permission list) is never sent: the card says the answer wasn't read and points at `/lihat`. A `/` command that isn't the bot's (`/exit`, `/compact`, `/clear`, `/model`) goes to the pane, reply-to-card included.
- Agents can ask to close any pane in their workspace, not only a helper they opened: `gembala close <pane>` waits as a card in the asking pane (close it or leave it) and the asking agent gets the answer through `gembala wait`.
- A dropped connection no longer greys out every pane with "This pane stopped because the daemon stopped or was restarted": Gembala asks the daemon which run it is, and when it is the same one the panes are re-attached where they were, their screen rebuilt from the daemon (no Restart click, so no agent opened twice). The same happens to a pane that fell behind a big burst of output: it is re-attached instead of having its connection cut.
- Telegram: phone text follows the last pane only while an agent still runs in it, so a reply to a card an agent posted through the bot itself (no pane name on its first line) no longer lands on the shell that pane became; a shell pane's refusal now names the pane.
- Fixes: a Claude pane no longer stays on "needs permission" after the prompt is gone; a second click on "Open and send" no longer opens another pane.
- A Claude pane whose session file reads idle no longer stays on "needs permission" when the prompt hook came from a background agent.
- Settings: Esc closes the add-account form without closing Settings; the form stays closed until "+ Add account" is pressed.
- Settings covers the whole window again: the sidebar's resize line no longer cuts through it, and the usage rail dims behind it.
- The bell starts empty after opening the app: a pane that finished while Gembala was closed no longer opens a bell entry or a banner, and a pane on screen in the front window never adds to the badge.

### Indonesia

- Agent bisa minta model dan effort waktu minta lu buka pane baru. Pilih model dari daftar atau isi ID-nya sendiri di kartu konfirmasi; pane agent yang udah kebuka nggak ikut berubah.
- Notifikasi sekarang bilang siapa yang butuh lu dan kenapa: pane dan agent-nya, apa yang ditunggu (izin, jawaban, password, persetujuan lu, atau kegagalan), atau bahwa tugasnya selesai, plus apa yang perlu lu lakukan. Satu banner per pane; banner hilang setelah lu menjawab, dan pane yang selesai barengan berbagi satu banner. Tiap jenis punya saklar sendiri di Pengaturan › Umum › Notifikasi.
- Laporkan masalah (⌘P › Laporkan masalah): Gembala isi versi buat lu, lu cek teksnya, lalu Gembala buka issue GitHub baru di browser. Belum ada yang dikirim sampai lu menekan Submit di sana. Agent juga bisa nyiapin laporan lewat `gembala report`.
- Sisa jatah per login agent (Claude, Codex, Oh My Pi, Gemini) sekarang tampil di sisi pane.
- Pane helper yang dibuka agent tampil sebagai sub-tab di bawah tab-nya, lengkap dengan tahap dan status.
- Bar pane menyesuaikan lebar; yang nggak muat pindah ke menu "…".
- Lebih ringan: scrollback bawaan sekarang 2.000 baris (tiap baris memakai memori di setiap pane aktif). Kalau sebelumnya lu sudah mengatur angka lebih besar, pengaturan itu tetap dipakai. Lihat Pengaturan › Tampilan.
- Telegram: kartu izin menjelaskan apa yang diminta agent dan alasannya, dengan tombol tepat di bawah teks (Oh My Pi, OpenCode, Claude, Codex). Kartu selesai Codex menunggu jawaban agent, bukan menampilkan layarnya. Dialog ambil web Claude sekarang dihitung sebagai "butuh lu".
- Telegram: kartu "selesai" sekarang membawa jawaban agent untuk Claude, Codex, dan Oh My Pi (layar pane cuma dipakai kalau agent belum punya hook jawaban). Baris pemanggilan tool Claude tidak dikirim ke ponsel. Layar yang menampilkan pemilih atau dialog (misalnya `/model` Claude atau daftar izin) tidak dikirim; kartu bilang jawabannya belum dibaca dan mengarahkan lu ke `/lihat`. Perintah `/` selain milik bot (`/exit`, `/compact`, `/clear`, `/model`) diteruskan ke pane, termasuk saat membalas kartu.
- Agent sekarang bisa minta pane mana pun di workspace-nya ditutup, bukan cuma helper yang dia buka: permintaannya jadi kartu di pane yang meminta (tutup atau biarkan), lalu jawaban lu dikirim lewat `gembala wait`.
- Koneksi putus tidak lagi membuat semua pane abu-abu dengan pesan "This pane stopped because the daemon stopped or was restarted": Gembala menanyakan daemon mana yang aktif, lalu menyambungkan kembali pane kalau prosesnya sama dan membangun ulang layarnya dari daemon. Lu nggak perlu menekan Restart, jadi agent tidak terbuka dua kali. Pane yang tertinggal setelah banyak output juga disambungkan ulang, bukan diputus.
- Telegram: teks ponsel sekarang mengikuti pane terakhir hanya selama agent masih berjalan di sana. Jadi balasan ke kartu dari bot itu sendiri (baris pertama tanpa nama pane) nggak nyasar ke shell yang sudah menggantikan pane tadi; penolakan shell juga sekarang menyebut nama pane.
- Perbaikan: pane Claude tidak lagi terus menampilkan "butuh izin" setelah dialognya hilang; klik kedua "Buka dan kirim" tidak lagi membuka pane tambahan.
- Pane Claude yang file sesinya terbaca idle sekarang tidak lagi terus "butuh izin" saat prompt hook berasal dari agent latar.
- Pengaturan: Esc menutup formulir tambah akun tanpa ikut menutup Pengaturan; formulir tetap tertutup sampai lu menekan "+ Tambah akun".
- Pengaturan kembali menutupi seluruh jendela: garis ubah ukuran sidebar tidak lagi menembusnya, dan panel penggunaan meredup di belakang.
- Lonceng sekarang kosong setelah app dibuka: pane yang selesai saat Gembala tertutup tidak lagi membuat entri lonceng atau banner, dan pane yang tampil di jendela depan tidak menambah badge.

## 0.1.8

- Safety fix for Telegram permission buttons: Claude Code 2.1.311 asks with 4 options in a Bash dialog, and button "3" used to switch the session to auto mode. Buttons now come from the options on screen with their own words ("4. No"); options that change the permission mode never get a button, and an unknown dialog only gets Esc.
- First start asks "Do you usually use a terminal?" and picks Simple or Full mode (Settings › General). Simple uses everyday words and folds technical details.
- "+ Ask an agent" (⌘N) at the top of the sidebar. The welcome card opens each agent you have, or helps install one (types the install command into a new terminal tab for you to run).
- A sentence typed into the terminal by mistake ("command not found") offers to send it to an agent instead.
- The Ask an agent dialog starts with "I steer" for new users and shows agents by name.
- Approval cards in a pane stack instead of covering each other; the message drawer can answer "open an agent" requests too.

## 0.1.7

- Agents can close the helper panes they opened (`gembala close <pane>`), and a helper that sits idle 15 minutes after its reply closes by itself (Settings › General). Fewer idle agents eating RAM.
- Harness: a run that finishes closes its stage panes; a stopped or failed run keeps them, with a "Close stage panes" button.
- The Telegram command menu follows the installed version (`/to <pane> <text>` sends straight to a pane).
- Codex in auto mode uses its own reviewer (`--approve-for-me`) instead of never asking.
- Codex 0.167 and Oh My Pi 18.18 are marked tested; New task reminds you that a new folder asks for trust first.

## 0.1.6

- Codex panes get a status line like Claude's (model, context, 5-hour and weekly limits as bars).
- Telegram: permission buttons for Codex, the question shows on Claude's permission card, `/exit` says the agent left, a folder-trust dialog shows as "needs you".
- Tabs whose panes are gone close properly; Harness fixes (Codex stages, folder trust, fallback models).
