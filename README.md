# Gembala (app)

Kode aplikasi. BRD + mockup: repo gembala-brd (`BRD.md` = sumber utama).

## Struktur

```
core/              binary `gembala`: `gembala daemon` + CLI (send/wait/read/reply/hook)
  src/rundir.rs    folder socket privat (SEC-1, SEC-25)
ui/                Tauri 2 + SvelteKit (static) + Svelte 5
  src-tauri/       klien daemon saja, TIDAK membuka PTY (R-5); CSP + capabilities minimal (SEC-24)
  src/             tampilan; data dari daemon hanya lewat binding teks, tanpa {@html}
```

Menyusul saat dibutuhkan: `adapters/` (file hook per agent, SEC-26) dan `tests/e2e/` (kriteria BRD bagian 17).

## Jalankan

Butuh Rust (`brew install rustup && rustup default stable`) dan Node.

```
cargo test -p gembala              # core
cargo run -p gembala -- daemon     # cek folder socket
cd ui && npm install --ignore-scripts && npm run tauri dev
```

Instal npm selalu dengan `--ignore-scripts` (SEC-16).
