//! Grid VT per pane di RAM: snapshot attach (FR-2), teks polos untuk `read` (FR-7).

use std::num::NonZeroU16;

/// Parser, then the prefix of a sequence/UTF-8 char cut off at the end of the stream (see `pending`).
pub struct Grid(vt100::Parser, Vec<u8>);

/// Start of the incomplete tail of `b`: an escape sequence (CSI, OSC, ESC + intermediates) or a cut-off UTF-8
/// char; `b.len()` if the end is intact. The vt100 parser keeps that prefix inside itself, a snapshot does not.
fn pending(b: &[u8]) -> usize {
    let from = b.len().saturating_sub(4096);
    if let Some(i) = b[from..].iter().rposition(|&c| c == 0x1b).map(|i| from + i) {
        let r = &b[i + 1..];
        let open = match r.first() {
            None => true,
            Some(b'[') => r[1..].iter().all(|c| (0x20..=0x3f).contains(c)),
            Some(b']' | b'P' | b'X' | b'^' | b'_') => !r.contains(&0x07),
            Some(0x20..=0x2f) => r.iter().all(|c| (0x20..=0x2f).contains(c)),
            _ => false,
        };
        if open {
            return i;
        }
    }
    let tail = &b[b.len().saturating_sub(3)..];
    let Some(k) = tail.iter().rposition(|&c| c >= 0xc0) else { return b.len() };
    let need = match tail[k] {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    if tail.len() - k < need { b.len() - (tail.len() - k) } else { b.len() }
}

/// vt100 menganggap bold dan dim saling ganti (`ESC[2m` saja dari bold ke dim), xterm menganggap keduanya terpisah:
/// tanpa `22` dulu, baris dim sesudah baris bold tampil bold+dim. Tiap parameter 1/2 didahului 22 (lewati warna 38/48).
fn fix_intensity(s: &str) -> String {
    let re = regex_lite::Regex::new("\x1b\\[([0-9;]*)m").unwrap();
    re.replace_all(s, |c: &regex_lite::Captures| {
        let ps: Vec<&str> = c[1].split(';').collect();
        let mut out: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < ps.len() {
            match ps[i] {
                "38" | "48" => {
                    let n = if ps.get(i + 1) == Some(&"5") { 3 } else { 5 };
                    out.extend(&ps[i..(i + n).min(ps.len())]);
                    i += n;
                    continue;
                }
                "1" | "2" => out.extend(["22", ps[i]]),
                p => out.push(p),
            }
            i += 1;
        }
        format!("\x1b[{}m", out.join(";"))
    })
    .into_owned()
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap()
}

impl Grid {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Grid {
        Grid(vt100::Parser::new(nz(rows), nz(cols), scrollback), Vec::new())
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
        let joined;
        let all = if self.1.is_empty() {
            bytes
        } else {
            joined = [self.1.as_slice(), bytes].concat();
            &joined
        };
        let p = pending(all);
        // ponytail: a prefix over 4 KiB (e.g. a long OSC 52) is dropped; the screen stays right, only its rest can leak
        self.1 = if all.len() - p <= 4096 { all[p..].to_vec() } else { Vec::new() };
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
        out.push_str(&fix_intensity(&s.state_formatted()));
        // the next pty chunk (a live event) continues the cut-off sequence; xterm has to be mid-sequence too
        let mut out = out.into_bytes();
        out.extend_from_slice(&self.1);
        out
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
        fix_intensity(&out).into_bytes()
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

    #[test]
    fn bold_ke_dim_diawali_22() {
        assert_eq!(fix_intensity("\x1b[2mx\x1b[1my"), "\x1b[22;2mx\x1b[22;1my");
        assert_eq!(fix_intensity("\x1b[38;2;1;2;3;1m"), "\x1b[38;2;1;2;3;22;1m");
        assert_eq!(fix_intensity("\x1b[38;5;2m\x1b[m"), "\x1b[38;5;2m\x1b[m");
    }

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

    /// A pty read can stop mid escape sequence or mid UTF-8 char; the rest arrives as a live event after the snapshot.
    /// Without its prefix in the snapshot, that rest is printed as text (`[240G`, `46m`, `�`).
    #[test]
    fn attach_di_tengah_sequence() {
        let stream = "\x1b[2Jhalo \x1b[1;32mhijau\x1b[0m ─ \x1b]0;judul\x07\x1b(B\x1b[3;5Hab\x1b[240Gz".as_bytes();
        let mut whole = Grid::new(5, 40, 0);
        whole.feed(stream);
        for cut in 1..stream.len() {
            let mut a = Grid::new(5, 40, 0);
            a.feed(&stream[..cut]);
            let mut b = Grid::new(5, 40, 0);
            b.feed(&a.replay());
            b.feed(&stream[cut..]);
            assert_eq!(b.text(), whole.text(), "dipotong di byte {cut}");
        }
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
        let teks = String::from_utf8_lossy(&b.before(b.position())).replace("\x1b[m", "").replace("\x1b[22;1m", "");
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
