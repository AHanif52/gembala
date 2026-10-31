//! Grid VT per pane di RAM: snapshot attach (FR-2), teks polos untuk `read` (FR-7).

use std::num::NonZeroU16;

pub struct Grid(vt100::Parser);

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap()
}

impl Grid {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Grid {
        Grid(vt100::Parser::new(nz(rows), nz(cols), scrollback))
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.0.set_size(nz(rows), nz(cols));
    }

    /// Byte yang ditulis ke terminal kosong berukuran sama supaya tampil identik:
    /// layar alternatif, isi + warna, posisi/visibilitas kursor, mode input
    /// (bracketed paste, mouse, application cursor/keypad, bentuk kursor).
    // ponytail: hanya layar terlihat; scrollback layar utama tidak ikut dikirim saat attach
    pub fn snapshot(&self) -> Vec<u8> {
        let s = self.0.screen();
        let mut out = String::new();
        if s.alternate_screen() {
            out.push_str("\x1b[?1049h");
        }
        out.push_str(&s.state_formatted());
        out.into_bytes()
    }

    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.0.screen().size();
        (r.get(), c.get())
    }

    #[allow(dead_code)] // dipakai `read` (FR-7) di Fase 2
    pub fn text(&self) -> String {
        self.0.screen().contents()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{read_to_end, Pty};
    use std::time::Duration;

    /// Attach ulang = snapshot A ditulis ke grid baru B; B harus sama persis dengan A.
    fn assert_roundtrip(a: &Grid) {
        let (rows, cols) = a.0.screen().size();
        let mut b = Grid::new(rows.get(), cols.get(), 0);
        b.feed(&a.snapshot());
        let (sa, sb) = (a.0.screen(), b.0.screen());
        assert_eq!(sb.alternate_screen(), sa.alternate_screen());
        assert_eq!(sb.cursor_position(), sa.cursor_position());
        assert_eq!(sb.state_formatted(), sa.state_formatted());
    }

    #[test]
    fn layar_utama_berwarna() {
        let mut g = Grid::new(10, 40, 100);
        g.feed(b"$ ls\r\n\x1b[1;32mhijau\x1b[0m biasa \x1b[48;5;196mmerah\x1b[0m\r\n\x1b[5;10Hdi tengah\x1b[3;1H");
        assert_roundtrip(&g);
        assert!(g.text().contains("di tengah"));
    }

    #[test]
    fn layar_alternatif_dan_mode() {
        let mut g = Grid::new(10, 40, 100);
        g.feed(b"prompt utama\r\n\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1h\x1b[?25l\x1b[2;3Hpanel TUI");
        assert!(g.0.screen().alternate_screen());
        assert_roundtrip(&g);
        assert!(!g.text().contains("prompt utama"));
    }

    #[test]
    fn resize_lalu_snapshot() {
        let mut g = Grid::new(10, 40, 100);
        g.feed(b"baris panjang yang nanti terpotong saat lebar dikecilkan");
        g.resize(5, 20);
        assert_roundtrip(&g);
    }

    /// Jalankan TUI nyata di PTY sebentar, lalu kirim SIGKILL.
    fn capture(cmd: &str, rows: u16, cols: u16, dur: Duration) -> Vec<u8> {
        let mut p = Pty::spawn(Some(cmd), rows, cols, &[]).unwrap();
        let r = p.master.try_clone().unwrap();
        let t = std::thread::spawn(move || read_to_end(r).unwrap());
        std::thread::sleep(dur);
        p.child.kill().unwrap();
        p.child.wait().unwrap();
        t.join().unwrap()
    }

    #[test]
    fn tui_nyata_vim() {
        let out = capture("vim -u NONE -N -c 'set number' /etc/shells", 24, 80, Duration::from_millis(800));
        let mut g = Grid::new(24, 80, 1000);
        g.feed(&out);
        assert!(g.0.screen().alternate_screen(), "vim harus di layar alternatif");
        assert!(g.text().contains("/bin/zsh"), "{}", g.text());
        assert_roundtrip(&g);
    }

    /// Manual: `GEMBALA_TUI='claude' cargo test -p gembala tui_nyata_env -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn tui_nyata_env() {
        let cmd = std::env::var("GEMBALA_TUI").unwrap_or_else(|_| "claude".into());
        let out = capture(&cmd, 40, 120, Duration::from_secs(4));
        let mut g = Grid::new(40, 120, 10_000);
        g.feed(&out);
        println!("{}", g.text());
        assert_roundtrip(&g);
    }
}
