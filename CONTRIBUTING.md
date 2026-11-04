# Berkontribusi ke Gembala

Kebutuhan (BRD) dan catatan teknis (TECH) dikelola pemilik repo di tempat terpisah yang tidak dibuka. Nomor seperti
FR-61, SEC-20, atau "TECH bagian 17" di kode dan dokumen merujuk ke sana. Isi yang lu perlu ada di issue: kerjakan
issue yang sudah ada, atau buka issue dulu sebelum fitur baru supaya dapat nomor FR dan kriteria terimanya.

## Siapkan mesin

macOS (Apple Silicon), Rust (`brew install rustup`), Node. Struktur kode, perintah uji, dan jebakan yang
sudah ketemu ada di [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

Build rilis ditandatangani sertifikat self-signed bernama **Gembala Dev** (`ui/src-tauri/tauri.conf.json`).
Tanpa sertifikat itu `npm run tauri build` gagal. Buat sekali di mesin lu:

```
cd "$(mktemp -d)"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Gembala Dev" \
  -addext "extendedKeyUsage=codeSigning" -addext "keyUsage=digitalSignature" \
  -keyout key.pem -out cert.pem
openssl pkcs12 -export -legacy -inkey key.pem -in cert.pem -out gembala-dev.p12 -passout pass:gembala
security import gembala-dev.p12 -k ~/Library/Keychains/login.keychain-db -P gembala -T /usr/bin/codesign
rm key.pem gembala-dev.p12
```

Sertifikat tiap mesin beda, jadi app hasil build lu hanya dipercaya daemon hasil build lu sendiri (SEC-20).
Uji otomatis dan `scripts/` memakai build debug, tidak butuh sertifikat.

Instal npm selalu `npm install --ignore-scripts` (SEC-16). Jangan tambah dependency tanpa dibahas di PR.

## Alur kerja

1. Branch dari `main`: `fr-61-pilih-akun`, `fix-seret-workspace`, dll.
2. Sebelum push, semua harus hijau:
   ```
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test -p gembala && cargo test -p ui
   cd ui && npm run check && npm test
   ```
3. Pesan commit: awali nomor FR/SEC bila ada, lalu ringkas dalam bahasa Indonesia
   (`FR-61: claude-<akun> di pane shell`). Pakai email noreply GitHub
   (`git config user.email <id>+<user>@users.noreply.github.com`).
4. Buka PR ke `main`. Perubahan UI: sertakan screenshot atau rekaman. Perubahan perilaku: sebut butir FR/kriteria
   yang terpenuhi dan uji yang menutupinya.
5. Temuan yang mengubah kebutuhan atau desain: tulis di PR/issue, pemilik yang memperbarui BRD/TECH.

## Keamanan

Celah keamanan jangan dibuka di issue publik; kirim lewat
[GitHub private vulnerability reporting](https://github.com/AHanif52/gembala/security/advisories/new).
