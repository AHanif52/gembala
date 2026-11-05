//! Per-pane VT grid in RAM: attach snapshot (FR-2), plain text for `read` (FR-7).

use std::num::NonZeroU16;

/// Parser, then the prefix of a sequence/UTF-8 char cut off at the end of the stream (see `pending`), then the raw
/// main-screen history (`Ring`) that attach replays, then the parser's scrollback length (kept for `rewind`).
pub struct Grid(vt100::Parser, Vec<u8>, Ring, usize);

/// Raw main-screen bytes per pane size, oldest first. ~140 bytes per coloured line against ~5 KB per line in vt100
/// scrollback, so attach can carry thousands of lines. Bytes while the alternate screen is on are dropped (a TUI
/// redraws itself constantly and has no history); its screen comes from `snapshot` instead.
pub struct Ring {
    pub segs: Vec<Seg>,
    len: usize,
    alt: bool,
    /// bytes ever kept; a mark (FR-56) is this count at one moment, so it stays valid while the front is trimmed
    total: u64,
}

/// Bytes written while the pane had this size. Replaying them at that size, then resizing, gives what an xterm that
/// stayed attached shows: cursor moves and wraps depend on the width.
pub struct Seg {
    pub rows: u16,
    pub cols: u16,
    pub data: Vec<u8>,
}

/// ~7,000 coloured lines; trimmed back to this once it is a quarter over, so the copy runs rarely.
const RING_MAX: usize = 1 << 20;
const ALT_ON: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
const ALT_OFF: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];

/// Earliest of `seqs` in `b`: (position, length).
fn find_any(b: &[u8], seqs: &[&[u8]]) -> Option<(usize, usize)> {
    seqs.iter().filter_map(|s| b.windows(s.len()).position(|w| w == *s).map(|i| (i, s.len()))).min()
}

impl Ring {
    fn new(rows: u16, cols: u16) -> Ring {
        Ring { segs: vec![Seg { rows, cols, data: Vec::new() }], len: 0, alt: false, total: 0 }
    }

    /// The first `n` kept bytes, segment by segment (empty segments keep their size).
    fn head(&self, mut n: usize) -> Vec<(u16, u16, &[u8])> {
        let mut out = Vec::new();
        for s in &self.segs {
            let k = n.min(s.data.len());
            out.push((s.rows, s.cols, &s.data[..k]));
            n -= k;
            if n == 0 && k < s.data.len() {
                break;
            }
        }
        out
    }

    /// Kept bytes before `mark`; a mark already trimmed away keeps nothing.
    fn before(&self, mark: u64) -> usize {
        self.len.saturating_sub(self.total.saturating_sub(mark) as usize)
    }

    /// Keeps main-screen bytes plus the enter/exit sequences themselves, so cursor save/restore still pairs up.
    // ponytail: a switch sequence split across two reads is missed; `feed` then resyncs from vt100's state
    fn push(&mut self, mut b: &[u8]) {
        loop {
            let seqs = if self.alt { &ALT_OFF } else { &ALT_ON };
            let Some((i, n)) = find_any(b, seqs) else {
                if !self.alt {
                    self.add(b);
                }
                return;
            };
            self.add(if self.alt { &b[i..i + n] } else { &b[..i + n] });
            self.alt = !self.alt;
            b = &b[i + n..];
        }
    }

    fn add(&mut self, b: &[u8]) {
        self.segs.last_mut().unwrap().data.extend_from_slice(b);
        self.len += b.len();
        self.total += b.len() as u64;
        if self.len > RING_MAX + RING_MAX / 4 {
            self.trim();
        }
    }

    /// Drops the oldest bytes down to `RING_MAX`, cutting after a newline and resetting attributes so the first kept
    /// line does not start mid sequence. An enter/exit pair has no newline between them, so it goes as a whole.
    fn trim(&mut self) {
        let mut over = self.len - RING_MAX;
        while over > 0 {
            let n = self.segs[0].data.len();
            if over >= n && self.segs.len() > 1 {
                over -= n;
                self.len -= n;
                self.segs.remove(0);
                continue;
            }
            let first = &mut self.segs[0];
            let from = over.min(n);
            let cut = first.data[from..].iter().position(|&c| c == b'\n').map_or(first.data.len(), |i| from + i + 1);
            self.len -= cut;
            first.data.drain(..cut);
            first.data.splice(0..0, *b"\x1b[m");
            self.len += 3;
            break;
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        let last = self.segs.last_mut().unwrap();
        if last.data.is_empty() {
            (last.rows, last.cols) = (rows, cols);
        } else if (last.rows, last.cols) != (rows, cols) {
            self.segs.push(Seg { rows, cols, data: Vec::new() });
        }
    }
}

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

/// vt100 treats bold and dim as swapping (`ESC[2m` alone goes from bold to dim), xterm treats them as separate:
/// without a `22` first, a dim line after a bold line shows bold+dim. Every 1/2 parameter is preceded by 22 (skipping colors 38/48).
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
        Grid(vt100::Parser::new(nz(rows), nz(cols), scrollback), Vec::new(), Ring::new(rows.max(1), cols.max(1)), scrollback)
    }

    /// Grid rebuilt from ring bytes (each segment at its size), then at `size`.
    fn from_ring(head: &[(u16, u16, &[u8])], size: (u16, u16), scrollback: usize) -> Grid {
        let first = head.first().map_or(size, |s| (s.0, s.1));
        let mut g = Grid::new(first.0, first.1, scrollback);
        for (rows, cols, data) in head {
            g.resize(*rows, *cols);
            g.feed(data);
        }
        g.resize(size.0, size.1);
        g
    }

    /// FR-56: ring position now. History "before the command/agent" is just this number, not a copy of the screen.
    pub fn mark(&self) -> u64 {
        self.2.total
    }

    /// FR-56: back to the history before `mark` (none = empty), parser rebuilt from it at the current size, so the
    /// block summary written next lands right under it. What came after (the agent's own output) is dropped.
    pub fn rewind(&mut self, mark: Option<u64>) {
        let keep = mark.map_or(0, |m| self.2.before(m));
        *self = Grid::from_ring(&self.2.head(keep), self.size(), self.3);
    }

    /// FR-62: main-screen lines (coloured, oldest first) before `mark` (none = up to now), at most `lines`, rebuilt
    /// from the ring. Empty while a TUI has the alternate screen, so the last log is kept instead of overwritten.
    pub fn history(&self, mark: Option<u64>, lines: usize) -> Vec<u8> {
        if mark.is_none() && self.0.screen().alternate_screen() {
            return Vec::new();
        }
        let keep = mark.map_or(self.2.len, |m| self.2.before(m));
        let mut g = Grid::from_ring(&self.2.head(keep), self.size(), lines);
        let at = g.position();
        g.before(at)
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
        self.2.push(bytes);
        self.2.alt = self.0.screen().alternate_screen();
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
        self.2.resize(rows.max(1), cols.max(1));
    }

    /// Bytes written to an empty terminal of the same size so it looks identical:
    /// alternate screen, content + colors, cursor position/visibility, input modes
    /// (bracketed paste, mouse, application cursor/keypad, cursor shape). No scrollback; see `replay`.
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

    /// FR-2: what attach writes, in order: each ring segment at its own size, then the tail at the current size.
    /// Main screen: the ring already ends in the live state, the tail only re-applies input modes set before the
    /// trimmed part (not when the ring ends mid sequence: the next live event finishes that one). Alternate screen: the
    /// ring stops at the switch, the tail is the snapshot of the TUI screen.
    pub fn replay(&self) -> (&[Seg], Vec<u8>) {
        let s = self.0.screen();
        let tail = if s.alternate_screen() {
            // a second ?1049h would save the alternate cursor over the main one, so leaving the TUI lands wrong
            let switched = self.2.segs.iter().rev().find(|g| !g.data.is_empty()).is_some_and(|g| ALT_ON.iter().any(|s| g.data.ends_with(s)));
            let snap = self.snapshot();
            if switched { snap.strip_prefix(b"\x1b[?1049h").map_or(snap.clone(), <[u8]>::to_vec) } else { snap }
        } else if self.1.is_empty() {
            s.input_mode_formatted().into_bytes()
        } else {
            Vec::new()
        };
        (&self.2.segs, tail)
    }

    /// `(scrollback length, cursor row)`: the current row's position in the main screen history.
    pub fn position(&self) -> (usize, u16) {
        let s = self.0.screen();
        (s.scrollback_len(), s.cursor_position().0)
    }

    /// FR-56: main screen rows (colored, oldest first) before the `position` recorded earlier, ready to write again to a
    /// new grid. The alternate screen has no history: empty.
    // ponytail: if the scrollback filled up since `position` was recorded, the dropped rows shift the slice
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

    /// Cursor row in color, then cursor column: the shell prompt still shows after the screen is replaced (FR-56 manual agent).
    /// The prompt's bracketed paste mode is restored too, so a multi-line paste doesn't run right away.
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

    /// Bottom screen row that has text (FR-4 screen pattern).
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

    /// What the UI does on attach: each segment at its size, then the tail at the current size.
    fn replayed(a: &Grid, scrollback: usize) -> Grid {
        let (segs, tail) = a.replay();
        let mut b = Grid::new(segs[0].rows, segs[0].cols, scrollback);
        for s in segs {
            b.resize(s.rows, s.cols);
            b.feed(&s.data);
        }
        let (rows, cols) = a.size();
        b.resize(rows, cols);
        b.feed(&tail);
        b
    }

    /// Re-attach = snapshot A written to a new grid B; B must match A exactly.
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
            let mut b = replayed(&a, 0);
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
        let mut b = replayed(&g, 100);
        assert_eq!(b.0.screen().contents(), g.0.screen().contents());
        assert_eq!(b.0.screen().cursor_position(), g.0.screen().cursor_position());
        assert!(b.before((b.0.screen().scrollback_len(), 0)).windows(7).any(|w| w == b"baris \x1b"), "riwayat ikut");
        let teks = String::from_utf8_lossy(&b.before(b.position())).replace("\x1b[m", "").replace("\x1b[22;1m", "");
        assert!(teks.starts_with("baris 0\r\nbaris 1\r\n") && teks.contains("baris 11"), "{teks}");
    }

    /// Ring: bytes in the alternate screen are dropped, the switch pair stays, a resize starts a new segment,
    /// and replay (ring + tail) gives the same screen, cursor and history as the live grid, in and out of a TUI.
    #[test]
    fn ring_tanpa_layar_alternatif() {
        let mut g = Grid::new(5, 30, 100);
        g.feed(b"satu\r\ndua\r\n\x1b[?1049h\x1b[2J\x1b[Htui berat");
        g.resize(6, 20);
        g.feed(b"\x1b[Hgambar ulang tui\x1b[?1049ltiga\r\n");
        let (segs, _) = g.replay();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].data, b"satu\r\ndua\r\n\x1b[?1049h");
        assert_eq!((segs[1].rows, segs[1].cols, &segs[1].data[..]), (6, 20, &b"\x1b[?1049ltiga\r\n"[..]));
        let b = replayed(&g, 100);
        assert_eq!(b.0.screen().contents(), g.0.screen().contents());
        assert_eq!(b.0.screen().cursor_position(), g.0.screen().cursor_position());

        // still inside the TUI: tail is the snapshot without a second ?1049h; leaving it restores the main cursor
        let mut g = Grid::new(5, 30, 100);
        g.feed(b"satu\r\ndua\x1b[?1049h\x1b[2;3Hpanel");
        let mut b = replayed(&g, 100);
        assert_eq!(b.0.screen().contents(), g.0.screen().contents());
        g.feed(b"\x1b[?1049lx");
        b.feed(b"\x1b[?1049lx");
        assert_eq!(b.0.screen().contents(), g.0.screen().contents());
        assert_eq!(b.0.screen().cursor_position(), g.0.screen().cursor_position());
    }

    #[test]
    fn ring_dipangkas_di_baris() {
        let mut g = Grid::new(5, 30, 0);
        g.feed(b"\x1b[1mtebal");
        for i in 0..RING_MAX / 5 {
            g.feed(format!("{i:08}\r\n").as_bytes());
        }
        let (segs, _) = g.replay();
        let d = &segs[0].data;
        assert!(g.2.len <= RING_MAX + RING_MAX / 4 && g.2.len == d.len());
        assert!(d.starts_with(b"\x1b[m") && d[3..].iter().take(8).all(u8::is_ascii_digit), "cut after a newline, reset first");
        assert!(!d.windows(5).any(|w| w == b"tebal"));
    }

    /// FR-56: rewind to a mark keeps the history before it, drops what came after (the agent), and the rebuilt
    /// screen equals one that only ever saw that history; a mark trimmed off the front keeps nothing.
    #[test]
    fn rewind_ke_penanda() {
        let mut g = Grid::new(4, 30, 0);
        g.feed(b"satu\r\ndua\r\n");
        let at = g.mark();
        g.feed(b"\x1b[2J\x1b[3J\x1b[Hagent menulis\r\nlagi\r\n");
        g.rewind(Some(at));
        g.feed(b"blok ringkas\r\n");
        let mut want = Grid::new(4, 30, 0);
        want.feed(b"satu\r\ndua\r\nblok ringkas\r\n");
        assert_eq!(g.text(), want.text());
        assert_eq!(g.0.screen().cursor_position(), want.0.screen().cursor_position());
        assert_eq!(g.replay().0[0].data, b"satu\r\ndua\r\nblok ringkas\r\n");

        let mut g = Grid::new(4, 30, 0);
        let at = g.mark();
        for i in 0..RING_MAX / 5 {
            g.feed(format!("{i:08}\r\n").as_bytes());
        }
        g.rewind(Some(at));
        assert!(g.replay().0.iter().all(|s| s.data.is_empty()) && g.text().trim().is_empty());
    }

    /// FR-62: log lines rebuilt from the ring, at most `lines`, before a mark; none while a TUI is on screen.
    #[test]
    fn history_dari_ring() {
        let mut g = Grid::new(4, 30, 0);
        for i in 0..20 {
            g.feed(format!("baris \x1b[1m{i}\x1b[m\r\n").as_bytes());
        }
        let plain = |b: Vec<u8>| String::from_utf8_lossy(&b).replace("\x1b[m", "").replace("\x1b[22;1m", "");
        let all = plain(g.history(None, 1000));
        assert!(all.starts_with("baris 0\r\n") && all.contains("baris 19\r\n"), "{all}");
        // `lines` of scrollback plus the screen rows above the cursor (4 rows, cursor on the last): 12..19
        assert!(plain(g.history(None, 5)).starts_with("baris 12\r\n"), "only the last lines");
        let at = g.mark();
        g.feed(b"agent\r\n\x1b[?1049hTUI");
        assert!(g.history(None, 1000).is_empty(), "TUI on screen: keep the last log");
        assert!(!plain(g.history(Some(at), 1000)).contains("agent"));
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

    /// Run a real TUI in a PTY briefly, then send SIGKILL.
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
        let chunk = line.repeat(160); // ~16 KB, about one PTY read
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
