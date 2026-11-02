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
    /// (bracketed paste, mouse, application cursor/keypad, bentuk kursor). Tanpa scrollback; lihat `replay`.
    pub fn snapshot(&self) -> Vec<u8> {
        let s = self.0.screen();
        let mut out = String::new();
        if s.alternate_screen() {
            out.push_str("\x1b[?1049h");
        }
        out.push_str(&s.state_formatted());
        out.into_bytes()
    }

    /// FR-2: scrollback layar utama lalu snapshot, supaya riwayat ikut tampil saat app dibuka lagi.
    /// Baris scrollback didorong keluar layar dengan baris baru sebelum snapshot menggambar ulang layar.
    // ponytail: baris panjang yang terbungkus jadi baris terpisah (tanpa reflow saat lebar berubah)
    pub fn replay(&mut self) -> Vec<u8> {
        let n = self.0.screen().scrollback_len();
        let rows = self.size().0 as usize;
        let mut out = if self.0.screen().alternate_screen() { Vec::new() } else { self.before((n, 0)) };
        if !out.is_empty() {
            out.extend(b"\r\n".repeat(rows));
        }
        out.extend(self.snapshot());
        out
    }

    /// `(panjang scrollback, baris kursor)`: posisi baris sekarang di riwayat layar utama.
    pub fn position(&self) -> (usize, u16) {
        let s = self.0.screen();
        (s.scrollback_len(), s.cursor_position().0)
    }

    /// FR-56: baris layar utama (berwarna, tertua dulu) sebelum `position` yang dicatat tadi, siap ditulis ulang ke
    /// grid baru. Layar alternatif tidak punya riwayat: kosong.
    // ponytail: bila scrollback sudah penuh sejak `position` dicatat, baris yang terbuang menggeser potongan
    pub fn before(&mut self, (sb, row): (usize, u16)) -> Vec<u8> {
        let s = self.0.screen_mut();
        if s.alternate_screen() {
            return Vec::new();
        }
        let (rows, cols) = (s.size().0.get() as usize, s.size().1.get());
        let mut keep = sb + row as usize;
        let mut out = String::new();
        let mut off = s.scrollback_len();
        while keep > 0 {
            s.set_scrollback(off);
            let take = if off > 0 { off.min(rows) } else { rows }.min(keep);
            for line in s.rows_formatted(0, cols).take(take) {
                out.push_str(&line);
                out.push_str("\x1b[m\r\n");
            }
            keep -= take;
            if off == 0 {
                break;
            }
            off -= off.min(rows);
        }
        s.set_scrollback(0);
        out.into_bytes()
    }

    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.0.screen().size();
        (r.get(), c.get())
    }

    pub fn text(&self) -> String {
        self.0.screen().contents()
    }

    /// Baris kursor berwarna lalu kolom kursor: prompt shell tetap tampil setelah layar diganti (FR-56 agent manual).
    /// Mode bracketed paste prompt ikut dipulihkan, supaya tempelan banyak baris tidak langsung dijalankan.
    pub fn cursor_line(&self) -> Vec<u8> {
        let s = self.0.screen();
        let (row, col) = s.cursor_position();
        let mut out = s.rows_formatted(0, s.size().1.get()).nth(row as usize).unwrap_or_default();
        out.push_str(&format!("\x1b[{}G", col + 1));
        if s.bracketed_paste() {
            out.push_str("\x1b[?2004h");
        }
        out.into_bytes()
    }

    /// Baris terbawah layar yang berisi teks (FR-4 pola layar).
    pub fn tail(&self, n: usize) -> String {
        let all = self.text();
        let lines: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
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
    fn replay_membawa_scrollback() {
        let mut g = Grid::new(5, 30, 100);
        for i in 0..12 {
            g.feed(format!("baris \x1b[1m{i}\x1b[m\r\n").as_bytes());
        }
        let mut b = Grid::new(5, 30, 100);
        b.feed(&g.replay());
        assert_eq!(b.0.screen().contents(), g.0.screen().contents());
        assert_eq!(b.0.screen().cursor_position(), g.0.screen().cursor_position());
        assert!(b.before((b.0.screen().scrollback_len(), 0)).windows(7).any(|w| w == b"baris \x1b"), "riwayat ikut");
        let teks = String::from_utf8_lossy(&b.before(b.position())).replace("\x1b[m", "").replace("\x1b[1m", "");
        assert!(teks.starts_with("baris 0\r\nbaris 1\r\n") && teks.contains("baris 11"), "{teks}");
    }

    #[test]
    fn before_memotong_di_posisi() {
        let mut g = Grid::new(4, 30, 100);
        g.feed(b"satu\r\ndua\r\ntiga\r\n");
        let at = g.position();
        g.feed(b"agent\r\nlayar\r\npenuh\r\nlagi\r\n");
        let teks = String::from_utf8_lossy(&g.before(at)).replace("\x1b[m", "");
        assert_eq!(teks, "satu\r\ndua\r\ntiga\r\n");
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
        let mut p = Pty::spawn(Some(cmd), rows, cols, &[], None).unwrap();
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

    /// Manual (release): `cargo test --release -p gembala bench_feed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_feed() {
        let line = format!("{}\r\n", "a".repeat(100));
        let chunk = line.repeat(160); // ~16 KB, seukuran baca PTY
        for sb in [0, 1_000, 10_000] {
            let mut g = Grid::new(40, 120, sb);
            let t = std::time::Instant::now();
            for _ in 0..3200 {
                g.feed(chunk.as_bytes()); // ~52 MB
            }
            let dt = t.elapsed().as_secs_f64();
            println!("scrollback {sb:>6}: {:.0} MB/s", 52.0 / dt);
        }
        let rss = || {
            let mut u: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
            u.ru_maxrss as f64 / 1e6 // macOS: byte
        };
        let before = rss();
        let mut g = Grid::new(40, 120, 10_000);
        for _ in 0..100 {
            g.feed(chunk.as_bytes());
        }
        println!("maxrss naik {:.0} MB untuk 1 grid scrollback 10.000 penuh", rss() - before);
        for cmd in ["head -c 52428800 /dev/zero | tr '\\0' 'a' | fold -w 100 > /dev/null", "head -c 52428800 /dev/zero | tr '\\0' 'a' | fold -w 100", "yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 52428800"] {
            let t = std::time::Instant::now();
            let mut p = Pty::spawn(Some(cmd), 40, 120, &[], None).unwrap();
            let n = read_to_end(&p.master).unwrap().len();
            p.child.wait().unwrap();
            println!("PTY {:.1} MB dalam {:.2} s: {}", n as f64 / 1e6, t.elapsed().as_secs_f64(), &cmd[cmd.len() - 20..]);
        }
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
