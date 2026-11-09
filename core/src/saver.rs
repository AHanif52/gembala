//! FR-27, SEC-31, T-17 (TECH 13): the token saver, pure functions only. Order matters: strip ANSI, merge identical
//! lines, cut long output keeping head, tail and every error line, then always append the exit code. Error lines are
//! never dropped, so a filtered log can never look green (SEC-31); pass conditions stay on the exit code and on the
//! original artifact, never on this text.

/// Lines kept at the start and at the end when the output is long (TECH 13).
const HEAD: usize = 40;
const TAIL: usize = 80;
/// Output above this many lines is cut (TECH 13).
const MAX_LINES: usize = 200;

/// Drop ANSI escape sequences: CSI (`ESC [ … final`), OSC/DCS/SOS/PM/APC (`ESC ] … BEL` or `ESC \`), and other
/// two-byte escapes. Bytes that are not part of an escape sequence are copied as they are, so UTF-8 stays valid.
pub fn strip_ansi(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x1b {
            // copy the whole UTF-8 sequence through `char_indices`-free slicing: find the next ESC, done below
            let start = i;
            while i < b.len() && b[i] != 0x1b {
                i += 1;
            }
            out.push_str(std::str::from_utf8(&b[start..i]).unwrap_or_default());
            continue;
        }
        i += 1;
        let Some(&kind) = b.get(i) else { break };
        match kind {
            b'[' => {
                // CSI: parameter/intermediate bytes, then one final byte 0x40..=0x7e
                i += 1;
                while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
                i = (i + 1).min(b.len());
            }
            b']' | b'P' | b'X' | b'^' | b'_' => {
                // string sequences end with BEL or ST (ESC \)
                i += 1;
                while i < b.len() {
                    if b[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            0x20..=0x2f => {
                // nF escape: intermediates then a final byte
                i += 1;
                while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
                    i += 1;
                }
                i = (i + 1).min(b.len());
            }
            _ => i += 1,
        }
    }
    out
}

/// `at foo/bar.js:12:3` (and the same with a trailing `)`) is a stack frame: always kept.
fn is_stack_frame(line: &str) -> bool {
    if !line.contains("at ") {
        return false;
    }
    let tail = line.trim_end().trim_end_matches(')').trim_end();
    tail.rsplit_once(':').is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// SEC-31, T-17: the patterns TECH 13 lists. Never cut these lines, whatever their position.
fn is_error_line(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    low.contains("error") || low.contains("failed") || low.contains("panic") || low.contains("fail") || low.contains("traceback") || is_stack_frame(line)
}

/// FR-27 filter (TECH 13). `exit` = the command's exit code when there is one; it is always reported, even when the
/// output itself is empty. Lines dropped from the middle are replaced by one marker per gap.
pub fn reduce(text: &str, exit: Option<i32>) -> String {
    let plain = strip_ansi(text);
    let mut merged: Vec<(String, usize)> = Vec::new();
    for raw in plain.split('\n') {
        let line = raw.trim_end_matches('\r').to_string();
        match merged.last_mut() {
            Some((prev, n)) if *prev == line => *n += 1,
            _ => merged.push((line, 1)),
        }
    }
    let mut lines: Vec<String> = merged.into_iter().map(|(l, n)| if n > 1 { format!("{l} (×{n})") } else { l }).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.len() > MAX_LINES {
        let mut keep = vec![false; lines.len()];
        for f in keep.iter_mut().take(HEAD) {
            *f = true;
        }
        for f in &mut keep[lines.len() - TAIL..] {
            *f = true;
        }
        for (i, l) in lines.iter().enumerate() {
            if is_error_line(l) {
                keep[i] = true;
            }
        }
        let mut cut = Vec::with_capacity(lines.len());
        let mut omitted = 0usize;
        for (i, line) in lines.into_iter().enumerate() {
            if keep[i] {
                if omitted > 0 {
                    cut.push(format!("[... {omitted} lines omitted ...]"));
                    omitted = 0;
                }
                cut.push(line);
            } else {
                omitted += 1;
            }
        }
        if omitted > 0 {
            cut.push(format!("[... {omitted} lines omitted ...]"));
        }
        lines = cut;
    }
    let mut out = lines.join("\n");
    if let Some(code) = exit {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("exit code: {code}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Criterion 14: rough token count = characters / 4.
    fn approx_tokens(text: &str) -> usize {
        text.chars().count() / 4
    }

    fn log_500() -> String {
        let mut s = String::new();
        for i in 1..=500 {
            if i == 250 {
                s.push_str("\x1b[31mFAIL: expected 2, got 3\x1b[0m\r\n");
            } else {
                s.push_str(&format!("ok {i}\r\n"));
            }
        }
        s
    }

    #[test]
    fn t17_fail_in_the_middle_survives_with_exit_code() {
        let out = reduce(&log_500(), Some(4));
        assert!(out.contains("FAIL: expected 2, got 3"), "{out}");
        assert!(out.contains("exit code: 4"), "{out}");
        assert!(out.contains("lines omitted"), "{out}");
        assert!(out.starts_with("ok 1"), "{}", &out[..40.min(out.len())]);
        assert!(out.trim_end().lines().any(|l| l == "ok 500"), "{out}");
        assert!(!out.contains("\x1b"), "ANSI left behind");
    }

    #[test]
    fn criterion_14_saver_on_is_smaller_than_off() {
        let raw = log_500();
        let on = reduce(&raw, Some(0));
        let off = raw.replace("\r\n", "\n");
        let (a, b) = (approx_tokens(&on), approx_tokens(&off));
        // measured in the H6 report; the ratio only has to be clearly below 1
        println!("criterion 14: on = {a} tokens, off = {b} tokens");
        assert!(a < b, "on {a} >= off {b}");
        assert!(a * 2 < b, "on {a} not clearly below off {b}");
    }

    #[test]
    fn ansi_and_repeated_lines() {
        let text = "\x1b]0;title\x07\x1b[1mbuild\x1b[0m\nbuild\nbuild\n\n\x1b[2mdone\x1b[0m\n";
        assert_eq!(reduce(text, None), "build (×3)\n\ndone");
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x07"), "red link");
    }

    #[test]
    fn short_log_keeps_every_line() {
        let out = reduce("a\nb\nb\nerror: nope\n", Some(1));
        assert_eq!(out, "a\nb (×2)\nerror: nope\nexit code: 1");
        // output that is empty still reports how the command ended
        assert_eq!(reduce("", Some(127)), "exit code: 127");
        assert_eq!(reduce("", None), "");
    }

    #[test]
    fn stack_traces_are_kept() {
        let mut text = String::from("Traceback (most recent call last):\n");
        for _ in 0..300 {
            text.push_str("filling\n");
        }
        text.push_str("    at run (/srv/app.js:12:3)\n");
        let out = reduce(&text, Some(1));
        assert!(out.contains("Traceback (most recent call last):"), "{out}");
        assert!(out.contains("at run (/srv/app.js:12:3)"), "{out}");
    }
}
