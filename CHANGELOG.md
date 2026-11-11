# Changelog

`scripts/rilis.sh` puts the section of the version being released on its GitHub release page.

## 0.1.9

### English

- Agents can request a model and effort when asking you to open a new pane. Choose a listed model or enter an ID in the confirmation card; a setting doesn't change an agent pane that's already open.
- The Open and send card lists models from the destination agent and login (Codex, Oh My Pi, OpenCode, Gemini). Claude keeps its latest-model aliases; entering a model ID always works, including when the CLI cannot list models.
- "Needs you" (sidebar): everything that waits for a person in one list, from any agent: a permission, an answer, a password, a folder to trust, a message to approve, an agent to open, or a harness gate. One sentence per row, longest wait on top, grouped by what it blocks; the row takes you to its pane or card. A pane that asks gets a bar above it with the agent's own question and the same buttons as the Telegram card (Claude, Codex, Oh My Pi, OpenCode; an agent whose dialog was never recorded says "Open to answer"). Cards in one pane stack (front card whole, the rest fold with "See"), and the message box is a column that narrows the pane instead of covering the buttons. Settings › General › Notifications turns the bar off; the list, bell and Telegram stay.
- A tab whose pane waits for your OK (a message, an agent to open, a harness gate) says "waiting for your OK" in the sidebar, and ⌘⇧U cycles to it too.
- Fewer approval cards: the harness messages its own stage panes, and an agent messages a helper it opened through a card you approved, without asking again.
- Simple mode names the agent, not shell-N, in the tab, the pane head and the message box.
- A tab split by hand lists each of its agents as its own row in the sidebar.
- After an upgrade, a "What's new" card shows this list once (and names releases you skipped); Esc or a click outside closes it.
- Panes no longer inherit `NO_COLOR` from the agent that started the daemon.
- A pane whose agent exited is a plain shell again: `gembala send` to it is refused instead of typing the message into zsh, and `gembala status` no longer lists it until an agent runs there again.
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
- Oh My Pi starts in auto mode (`--approval-mode=yolo`), from its profile and when typed in a zsh pane; a profile with `permission = "default"` keeps it asking.

### Indonesia

- Agent bisa minta model dan effort waktu minta lu buka pane baru. Pilih model dari daftar atau isi ID-nya sendiri di kartu konfirmasi; pane agent yang udah kebuka nggak ikut berubah.
- Kartu Buka dan kirim sekarang nampilin model dari agent dan login tujuan (Codex, Oh My Pi, OpenCode, Gemini). Claude tetap pakai alias model terbaru; ngetik ID model sendiri selalu bisa, termasuk kalau CLI-nya nggak bisa ngasih daftar.
- "Butuh lu" (sidebar): semua yang nunggu lu ada di satu daftar, dari agent mana pun: izin, jawaban, password, folder yang perlu dipercaya, pesan yang perlu disetujui, agent yang mau dibuka, atau gerbang harness. Satu kalimat per baris, yang paling lama nunggu di atas, dikelompokkan menurut apa yang ketahan; klik barisnya buat langsung ke pane atau kartunya. Pane yang nanya dapat bar di atasnya, isinya pertanyaan agent itu sendiri dan tombol yang sama dengan kartu Telegram (Claude, Codex, Oh My Pi, OpenCode; agent yang dialognya belum pernah direkam bilang "Buka buat jawab"). Kartu di satu pane ditumpuk (kartu depan utuh, sisanya dilipat dengan "Lihat"), dan kotak pesan jadi kolom yang menyempitkan pane, nggak lagi nutupin tombol. Bar bisa dimatikan di Pengaturan › Umum › Notifikasi; daftar, lonceng, dan Telegram tetap jalan.
- Tab yang pane-nya nunggu persetujuan lu (pesan, agent yang mau dibuka, gerbang harness) bilang "nunggu lu setujui" di sidebar, dan ⌘⇧U juga lompat ke situ.
- Kartu persetujuan lebih sedikit: harness kirim pesan ke pane tahapnya sendiri, dan agent kirim pesan ke helper yang dia buka lewat kartu yang udah lu setujui, tanpa nanya lagi.
- Mode Sederhana nyebut nama agent, bukan shell-N, di tab, kepala pane, dan kotak pesan.
- Tab yang lu belah sendiri nampilin tiap agent di dalamnya sebagai baris sendiri di sidebar.
- Habis upgrade, kartu "Yang baru" nampilin daftar ini sekali (plus nyebut rilis yang kelewat); Esc atau klik di luar kartu buat nutup.
- Pane nggak lagi ikut kebawa `NO_COLOR` dari agent yang nyalain daemon.
- Pane yang agent-nya udah keluar balik jadi shell biasa: `gembala send` ke situ ditolak, nggak lagi ngetik pesannya ke zsh, dan `gembala status` nggak nampilin pane itu sampai ada agent jalan lagi di sana.
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
- Oh My Pi langsung jalan di mode auto (`--approval-mode=yolo`), baik dari profilnya maupun waktu diketik di pane zsh; profil yang nulis `permission = "default"` tetap nanya.

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
