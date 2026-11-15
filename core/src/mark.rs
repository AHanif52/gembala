//! SEC-40: OSC 133 (block) and OSC 777 `gembala` (agent) markers carry a per-pane nonce. The hook scripts and the launch
//! command write `ESC ] 133 ; <nonce> ; D ; 3 BEL` / `ESC ] 777 ; <nonce> ; gembala ; agent-exit ; 0 BEL`. `MarkFilter`
//! sits between the PTY and everything else (grid, history, UI): a marker with the right nonce passes in its plain form
//! (`ESC ] 133 ; D ; 3 BEL`, so every parser downstream stays as it was); any other 133 or `777;gembala;` marker is
//! neutralized (its ESC becomes `␛`), so a pane that `cat`s a file full of markers can't fake block borders or an exit code.

const NEUTRAL: &str = "␛";
/// An OSC that stays open this long is no marker (a hook marker is a few hundred bytes at most).
const MAX_OSC: usize = 4096;

pub struct MarkFilter {
    nonce: Vec<u8>,
    /// Tail of the last read that may be the start of a marker (read boundary), joined with the next read.
    carry: Vec<u8>,
}

/// `ESC ] ` + number + `;` of the markers the filter looks at.
const PREFIXES: [&[u8]; 2] = [b"\x1b]133;", b"\x1b]777;"];
/// what follows an 8-bit OSC introducer in a forged marker
const C1_MARKS: [&[u8]; 2] = [b"133;", b"777;gembala;"];

impl MarkFilter {
    pub fn new(nonce: &str) -> Self {
        Self { nonce: nonce.as_bytes().to_vec(), carry: Vec::new() }
    }

    pub fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let joined;
        let data: &[u8] = if self.carry.is_empty() {
            input
        } else {
            joined = [std::mem::take(&mut self.carry).as_slice(), input].concat();
            &joined
        };
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while let Some(off) = data[i..].iter().position(|&c| c == 0x1b || c == 0x9d) {
            let p = i + off;
            out.extend_from_slice(&data[i..p]);
            let rest = &data[p..];
            if rest[0] == 0x9d {
                // 8-bit OSC introducer (U+009D, `C2 9D` in UTF-8; xterm.js reads it like `ESC ]`): a marker through this door is
                // always a forgery, the hooks write the 7-bit form
                let tail = &rest[1..];
                if C1_MARKS.iter().any(|x| x.starts_with(tail) && tail.len() < x.len()) {
                    // the lead byte already went out: take it back so the next read sees the whole U+009D
                    let lead = if out.last() == Some(&0xc2) { out.pop(); vec![0xc2] } else { Vec::new() };
                    self.carry = [lead.as_slice(), rest].concat();
                    return out;
                }
                if C1_MARKS.iter().any(|x| tail.starts_with(x)) {
                    if out.last() == Some(&0xc2) {
                        out.pop();
                    }
                    out.extend_from_slice(NEUTRAL.as_bytes());
                } else {
                    out.push(0x9d);
                }
                i = p + 1;
                continue;
            }
            // a read cut inside the prefix (even a lone ESC): hold it for the next read
            if rest.len() < 6 && PREFIXES.iter().any(|x| x.starts_with(rest)) {
                self.carry = rest.to_vec();
                return out;
            }
            let Some(prefix) = PREFIXES.iter().find(|x| rest.starts_with(x)) else {
                out.push(0x1b);
                i = p + 1;
                continue;
            };
            // end of the OSC: BEL, ST (`ESC \`), or another ESC (the string is aborted there, the ESC isn't consumed)
            let end = rest[prefix.len()..].iter().position(|&c| c == 0x07 || c == 0x1b).map(|e| prefix.len() + e);
            let (body_end, next) = match end {
                Some(e) if rest[e] == 0x07 => (e, p + e + 1),
                Some(e) if rest.get(e + 1) == Some(&b'\\') => (e, p + e + 2),
                Some(e) if e + 1 == rest.len() => {
                    self.carry = rest.to_vec();
                    return out;
                }
                Some(e) => (e, p + e),
                None if rest.len() < MAX_OSC => {
                    self.carry = rest.to_vec();
                    return out;
                }
                None => (rest.len(), p + rest.len()),
            };
            let body = &rest[prefix.len()..body_end];
            match self.accept(prefix, body) {
                Some(plain) => out.extend_from_slice(&plain),
                None if self.passes(prefix, body) => out.extend_from_slice(&rest[..next - p]),
                None => {
                    out.extend_from_slice(NEUTRAL.as_bytes());
                    out.extend_from_slice(&rest[1..body_end]);
                }
            }
            i = next;
        }
        // a read cut between the two bytes of U+009D
        if data.last() == Some(&0xc2) {
            self.carry = vec![0xc2];
            out.extend_from_slice(&data[i..data.len() - 1]);
            return out;
        }
        out.extend_from_slice(&data[i..]);
        out
    }

    /// The plain marker when `body` starts with this pane's nonce.
    fn accept(&self, prefix: &[u8], body: &[u8]) -> Option<Vec<u8>> {
        let rest = body.strip_prefix(self.nonce.as_slice())?.strip_prefix(b";")?;
        Some([prefix, rest, b"\x07"].concat())
    }

    /// OSC 777 of other tools (`notify`, …) isn't ours; only `777;gembala;…` without a nonce is a forgery.
    fn passes(&self, prefix: &[u8], body: &[u8]) -> bool {
        prefix == b"\x1b]777;" && !body.starts_with(b"gembala;")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: &str = "0123456789abcdef0123456789abcdef";

    fn run(chunks: &[&[u8]]) -> String {
        let mut f = MarkFilter::new(N);
        let mut out = Vec::new();
        for c in chunks {
            out.extend(f.feed(c));
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn marker_dengan_nonce_jadi_bentuk_polos() {
        let m = format!("a\x1b]133;{N};D;3\x07b\x1b]133;{N};A;/tmp\x07\x1b]777;{N};gembala;agent-exit;0\x07c");
        assert_eq!(run(&[m.as_bytes()]), "a\x1b]133;D;3\x07b\x1b]133;A;/tmp\x07\x1b]777;gembala;agent-exit;0\x07c");
        // ST ends a valid marker too, and comes out as BEL
        assert_eq!(run(&[format!("\x1b]133;{N};B\x1b\\").as_bytes()]), "\x1b]133;B\x07");
    }

    #[test]
    fn marker_palsu_dinetralkan() {
        for forged in ["\x1b]133;D;3\x07", "\x1b]133;D;3\x1b\\", "\x1b]133;wrong;D;3\x07", "\x1b]777;gembala;agent-exit;0\x07", "\x1b]133;;D;3\x07"] {
            let out = run(&[forged.as_bytes()]);
            assert!(!out.contains('\x1b'), "{forged:?} -> {out:?}");
            assert!(out.starts_with('␛'), "{out:?}");
        }
        // an OSC 777 of another tool and other OSCs pass untouched
        let other = "\x1b]777;notify;hi;there\x07\x1b]0;title\x07\x1b[31mred\x1b[0m";
        assert_eq!(run(&[other.as_bytes()]), other);
    }

    /// The 8-bit OSC introducer is no way around the filter (xterm.js reads U+009D like ESC ]).
    #[test]
    fn c1_osc_palsu_dinetralkan() {
        for forged in ["\u{9d}133;D;3\x07", "\u{9d}777;gembala;agent-exit;0\x07"] {
            let out = run(&[forged.as_bytes()]);
            assert!(!out.contains('\u{9d}') && out.starts_with('␛'), "{forged:?} -> {out:?}");
        }
        // cut between the two UTF-8 bytes, or inside the number
        assert!(!run(&[b"x\xc2", b"\x9d133;D;3\x07"]).contains('\u{9d}'));
        assert!(!run(&[b"\xc2\x9d13", b"3;D;3\x07"]).contains('\u{9d}'));
        // text that merely contains the byte or an unrelated C1 OSC passes
        assert_eq!(run(&["ŝ \u{9d}0;title\x07 end".as_bytes()]), "ŝ \u{9d}0;title\x07 end");
    }

    /// Dropping a forged marker must not let its neighbours join into a new one.
    #[test]
    fn marker_palsu_tak_bisa_disambung() {
        let out = run(&[b"\x1b]1\x1b]133;x\x0733;D;0\x07"]);
        assert!(!out.contains("\x1b]133;"), "{out:?}");
        let out = run(&[b"\x1b\x1b]133;D;1\x07"]);
        assert!(!out.contains("\x1b]133;"), "{out:?}");
        // a nested start inside a forged marker
        let out = run(&[b"\x1b]133;\x1b]133;D;1\x07"]);
        assert!(!out.contains("\x1b]133;"), "{out:?}");
    }

    /// The PTY read can cut a forged or a real marker anywhere.
    #[test]
    fn marker_terpotong_batas_baca() {
        let real = format!("x\x1b]133;{N};D;7\x07y");
        for cut in 0..real.len() {
            let (a, b) = real.as_bytes().split_at(cut);
            assert_eq!(run(&[a, b]), "x\x1b]133;D;7\x07y", "cut {cut}");
        }
        let forged = b"x\x1b]133;D;7\x07y";
        for cut in 0..forged.len() {
            let (a, b) = forged.split_at(cut);
            let out = run(&[a, b]);
            assert!(!out.contains("\x1b]133;"), "cut {cut}: {out:?}");
        }
    }

    #[test]
    fn osc_tanpa_ujung_tidak_ditahan_selamanya() {
        let mut big = b"\x1b]133;".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_OSC));
        let out = run(&[&big, b"tail"]);
        assert!(!out.contains('\x1b') && out.ends_with("tail"));
    }
}
