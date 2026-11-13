//! Pure Command Code contracts and v3 history projection (PLAN-CC-PANE, phase 1).
// The runner starts using these contracts in phase 2.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const LINE_MAX: usize = 8 * 1024 * 1024;

#[derive(Debug, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub struct RunResult {
    pub subtype: String,
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub duration_ms: Option<u64>,
    pub final_text: Option<String>,
    pub error: Option<Value>,
}

#[derive(Debug, PartialEq)]
pub enum Envelope {
    Event(Value),
    Result(RunResult),
}

fn string(v: &Value, key: &str) -> Result<String, String> {
    v[key].as_str().map(str::to_owned).ok_or_else(|| format!("missing or invalid {key}"))
}

fn optional_string(v: &Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(format!("invalid {key}")),
    }
}

fn optional_u64(v: &Value, key: &str) -> Result<Option<u64>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n.as_u64().map(Some).ok_or_else(|| format!("invalid {key}")),
    }
}

fn object(line: &[u8]) -> Result<Value, String> {
    if line.len() > LINE_MAX {
        return Err("Command Code line exceeds 8 MiB".into());
    }
    let text = std::str::from_utf8(line).map_err(|e| format!("invalid UTF-8: {e}"))?;
    let v: Value = serde_json::from_str(text).map_err(|e| format!("malformed JSON: {e}"))?;
    if !v.is_object() {
        return Err("expected JSON object".into());
    }
    Ok(v)
}

/// Buffers bytes until a complete line, so a UTF-8 character may span chunks.
#[derive(Default)]
pub struct Reader {
    pending: Vec<u8>,
    result_seen: bool,
    closed: bool,
    failure: Option<String>,
}

impl Reader {
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Envelope>, String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        if self.closed {
            return Err("stream already finished".into());
        }
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let n = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len());
            if self.pending.len() + n > LINE_MAX {
                return self.fail("Command Code line exceeds 8 MiB".into());
            }
            self.pending.extend_from_slice(&bytes[..n]);
            if n == bytes.len() {
                break;
            }
            match self.line() {
                Ok(Some(e)) => out.push(e),
                Ok(None) => {}
                Err(e) => return self.fail(e),
            }
            bytes = &bytes[n + 1..];
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Result<Vec<Envelope>, String> {
        let mut out = self.feed(&[])?;
        if !self.pending.is_empty() {
            match self.line() {
                Ok(Some(e)) => out.push(e),
                Ok(None) => {}
                Err(e) => return self.fail(e),
            }
        }
        if !self.result_seen {
            return self.fail("EOF without result".into());
        }
        self.closed = true;
        Ok(out)
    }

    fn fail<T>(&mut self, e: String) -> Result<T, String> {
        self.pending.clear();
        self.failure = Some(e.clone());
        Err(e)
    }

    fn line(&mut self) -> Result<Option<Envelope>, String> {
        let v = object(&std::mem::take(&mut self.pending))?;
        if self.result_seen {
            return Err("line after final result".into());
        }
        match string(&v, "type")?.as_str() {
            "event" => {
                let event = &v["event"];
                let kind = string(event, "type")?;
                // Unknown events are inert; no new status or permission semantics are inferred.
                let known = matches!(kind.as_str(),
                    "run_start" | "run_end" | "turn_start" | "turn_end" |
                    "message_start" | "text_delta" | "message_update" | "message_end" |
                    "thinking_start" | "thinking_delta" | "thinking_end" |
                    "model_request_start" | "model_request_end" | "model_trace" |
                    "tool_queued" | "tool_running" | "tool_update" | "tool_completed" |
                    "tool_errored" | "tool_denied" | "tool_hook_blocked" | "tool_hooks" |
                    "subagent_start" | "subagent_progress" | "subagent_stop" |
                    "api_retry" | "stream_restart" | "compaction_start" | "compaction_done" |
                    "compaction_outcome" | "notice" | "mod_error" | "skill_loaded" |
                    "session_titled" | "permission_mode_changed" | "config_setting_changed" |
                    "continuation_recovery" | "tool_input_coerced" | "tool_input_repaired" |
                    "interrupted" | "run_error");
                Ok(known.then(|| Envelope::Event(event.clone())))
            }
            "result" => {
                let subtype = string(&v, "subtype")?;
                if !matches!(subtype.as_str(), "success" | "error" | "max_turns") {
                    return Err("invalid result subtype".into());
                }
                let u = &v["usage"];
                if !u.is_null() && !u.is_object() {
                    return Err("invalid usage".into());
                }
                let result = RunResult {
                    subtype,
                    session_id: optional_string(&v, "sessionId")?,
                    stop_reason: optional_string(&v, "stopReason")?,
                    usage: Usage {
                        input_tokens: optional_u64(u, "inputTokens")?,
                        output_tokens: optional_u64(u, "outputTokens")?,
                        cache_read_tokens: optional_u64(u, "cacheReadTokens")?,
                        cache_write_tokens: optional_u64(u, "cacheWriteTokens")?,
                    },
                    duration_ms: optional_u64(&v, "durationMs")?,
                    final_text: optional_string(&v, "finalText")?,
                    error: v.get("error").cloned(),
                };
                self.result_seen = true;
                Ok(Some(Envelope::Result(result)))
            }
            _ => Err("unknown envelope type".into()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: Value, is_error: Option<bool> },
    Unsupported(String),
}

fn block(v: &Value) -> Result<Block, String> {
    Ok(match string(v, "type")?.as_str() {
        "text" => Block::Text(string(v, "text")?),
        "thinking" => Block::Thinking(string(v, "thinking")?),
        "tool_use" => Block::ToolUse {
            id: string(v, "id")?, name: string(v, "name")?,
            input: v.get("input").filter(|v| v.is_object()).cloned().ok_or("invalid tool input")?,
        },
        "tool_result" => Block::ToolResult {
            tool_use_id: string(v, "tool_use_id")?,
            content: v.get("content").filter(|v| v.is_string() || v.is_array()).cloned().ok_or("invalid tool result content")?,
            is_error: match v.get("is_error") {
                None => None,
                Some(Value::Bool(b)) => Some(*b),
                _ => return Err("invalid is_error".into()),
            },
        },
        kind => Block::Unsupported(kind.chars().take(128).collect()),
    })
}

#[derive(Debug)]
pub struct Message {
    pub role: String,
    pub content: Vec<Block>,
}

/// Non-message payload is deliberately omitted; custom entries are never rendered.
#[derive(Debug)]
pub struct Entry {
    pub kind: String,
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: Option<Message>,
}

fn entry(v: &Value) -> Result<Entry, String> {
    let kind = string(v, "type")?;
    let parent_id = match v.get("parentId") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        _ => return Err("missing or invalid parentId".into()),
    };
    let message = if kind == "message" {
        let m = &v["message"];
        Some(Message {
            role: string(m, "role")?,
            content: m["content"].as_array().ok_or("invalid message content")?.iter().map(block).collect::<Result<_, _>>()?,
        })
    } else { None };
    Ok(Entry { kind, id: string(v, "id")?, parent_id, timestamp: string(v, "timestamp")?, message })
}

#[derive(Debug)]
pub struct History {
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    pub entries: Vec<Entry>,
    /// An incomplete tail or broken ancestry is visible to the caller, never silently complete.
    pub partial: Option<String>,
}

/// Loads supplied bytes only. The caller owns file selection and filesystem access.
pub fn transcript(bytes: &[u8], leaf: Option<&str>) -> Result<History, String> {
    let mut lines = bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty());
    let header = object(lines.next().ok_or("missing session header")?)?;
    if header["type"] != "session" || header["version"] != 3 {
        return Err("format belum didukung; expected session v3".into());
    }
    let mut history = History {
        id: string(&header, "id")?, timestamp: string(&header, "timestamp")?,
        cwd: string(&header, "cwd")?, entries: Vec::new(), partial: None,
    };
    let mut entries = Vec::new();
    let mut index = HashMap::new();
    for line in lines {
        let e = match object(line).and_then(|v| entry(&v)) {
            Ok(e) => e,
            Err(e) => { history.partial = Some(e); break; }
        };
        if index.contains_key(&e.id) {
            history.partial = Some(format!("duplicate entry id: {}", e.id));
            break;
        }
        index.insert(e.id.clone(), entries.len());
        entries.push(e);
    }
    let mut next = leaf.map(str::to_owned).or_else(|| entries.last().map(|e| e.id.clone()));
    let mut seen = HashSet::new();
    let mut path = Vec::new();
    while let Some(id) = next {
        if !seen.insert(id.clone()) {
            history.partial = Some(format!("cycle at entry: {id}"));
            break;
        }
        let Some(&i) = index.get(&id) else {
            history.partial = Some(format!("missing parent or leaf: {id}"));
            break;
        };
        path.push(i);
        next = entries[i].parent_id.clone();
    }
    let mut entries: Vec<_> = entries.into_iter().map(Some).collect();
    history.entries = path.into_iter().rev().map(|i| entries[i].take().unwrap()).collect();
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const STREAM: &[u8] = include_bytes!("../tests/fixtures/commandcode/stream.ndjson");
    const SESSION: &[u8] = include_bytes!("../tests/fixtures/commandcode/session-v3.jsonl");

    fn read(bytes: &[u8]) -> Result<Vec<Envelope>, String> {
        let mut reader = Reader::default();
        let mut out = reader.feed(bytes)?;
        out.extend(reader.finish()?);
        Ok(out)
    }

    fn result(bytes: &[u8]) -> RunResult {
        match read(bytes).unwrap().pop().unwrap() {
            Envelope::Result(r) => r,
            _ => panic!("expected result"),
        }
    }

    fn tree(parents: &[(&str, Option<&str>)]) -> Vec<u8> {
        let mut lines = vec![json!({"type":"session","version":3,"id":"test","timestamp":"t","cwd":"/fixture"}).to_string()];
        lines.extend(parents.iter().map(|(id, parent)| json!({"type":"custom","id":id,"parentId":parent,"timestamp":"t"}).to_string()));
        lines.join("\n").into_bytes()
    }

    #[test]
    fn fixture_ignores_unknown_event_and_extra_fields() {
        let events = read(STREAM).unwrap();
        assert_eq!(events.len(), 4);
        let r = result(STREAM);
        assert_eq!(r.session_id.as_deref(), Some("fixture-session"));
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(r.usage.input_tokens, Some(12));
        assert_eq!(r.usage.cache_read_tokens, Some(0));
        assert_eq!(r.usage.cache_write_tokens, Some(0));
        assert_eq!(r.usage.output_tokens, Some(3));
        assert_eq!(r.duration_ms, Some(25));
        assert_eq!(r.final_text.as_deref(), Some("Halo 🌱"));
    }

    #[test]
    fn utf8_and_newlines_split_at_every_byte() {
        let expected = read(STREAM).unwrap();
        let mut reader = Reader::default();
        let mut actual = Vec::new();
        for byte in STREAM {
            actual.extend(reader.feed(&[*byte]).unwrap());
        }
        actual.extend(reader.finish().unwrap());
        assert_eq!(actual, expected);
        for split in 0..=STREAM.len() {
            let mut reader = Reader::default();
            let mut actual = reader.feed(&STREAM[..split]).unwrap();
            actual.extend(reader.feed(&STREAM[split..]).unwrap());
            actual.extend(reader.finish().unwrap());
            assert_eq!(actual, expected, "split at {split}");
        }
    }

    #[test]
    fn eof_drains_result_and_crlf_works() {
        assert_eq!(read(STREAM.strip_suffix(b"\n").unwrap()).unwrap(), read(STREAM).unwrap());
        let crlf = String::from_utf8(STREAM.to_vec()).unwrap().replace('\n', "\r\n");
        assert_eq!(read(crlf.as_bytes()).unwrap(), read(STREAM).unwrap());
    }

    #[test]
    fn early_error_has_no_session_or_stop_reason_or_usage() {
        let r = result(br#"{"type":"result","subtype":"error","error":{"message":"auth failed"}}"#);
        assert_eq!(r.session_id, None);
        assert_eq!(r.stop_reason, None);
        assert_eq!(r.usage, Usage::default());
        assert_eq!(r.error, Some(json!({"message":"auth failed"})));
        let r = result(br#"{"type":"result","subtype":"max_turns","usage":{"inputTokens":0}}"#);
        assert_eq!(r.usage.input_tokens, Some(0));
        assert_eq!(r.usage.output_tokens, None);
        assert_eq!(r.usage.cache_read_tokens, None);
        assert_eq!(r.usage.cache_write_tokens, None);
    }

    #[test]
    fn malformed_lines_and_shapes_are_errors() {
        for line in ["assistant text", "{", "[]", "null", "{}", "{\"type\":\"event\",\"event\":{}}",
            "{\"type\":\"result\",\"subtype\":\"other\"}",
            "{\"type\":\"result\",\"subtype\":\"success\",\"usage\":false}",
            "{\"type\":\"result\",\"subtype\":\"success\",\"usage\":{\"inputTokens\":-1}}",
            "{\"type\":\"result\",\"subtype\":\"success\",\"sessionId\":42}"] {
            assert!(read(line.as_bytes()).is_err(), "accepted {line}");
        }
        assert!(read(b"{\"type\":\"event\",\"event\":{\"type\":\"text_delta\",\"delta\":\"\xff\"}}\n").unwrap_err().contains("UTF-8"));
        assert!(read(b"\xc3").is_err());
    }

    #[test]
    fn line_limit_is_explicit_and_failure_is_sticky() {
        let mut reader = Reader::default();
        reader.feed(&vec![b' '; LINE_MAX]).unwrap();
        assert!(reader.feed(b"x").unwrap_err().contains("8 MiB"));
        assert!(reader.finish().unwrap_err().contains("8 MiB"));
        assert!(read(&vec![b' '; LINE_MAX + 1]).unwrap_err().contains("8 MiB"));
        let mut exact = br#"{"type":"result","subtype":"success"}"#.to_vec();
        exact.resize(LINE_MAX, b' ');
        assert!(read(&exact).is_ok());
    }

    #[test]
    fn eof_without_result_is_not_success() {
        for bytes in [b"".as_slice(), b"{\"type\":\"event\",\"event\":{\"type\":\"run_end\"}}\n"] {
            assert_eq!(read(bytes).unwrap_err(), "EOF without result");
        }
    }

    #[test]
    fn result_must_be_unique_and_last() {
        for tail in [b"{\"type\":\"result\",\"subtype\":\"success\"}\n".as_slice(),
            b"{\"type\":\"event\",\"event\":{\"type\":\"future_event\"}}\n"] {
            let bytes = [STREAM, tail].concat();
            assert!(read(&bytes).unwrap_err().contains("after final result"));
        }
        let mut reader = Reader::default();
        reader.feed(STREAM).unwrap();
        reader.finish().unwrap();
        assert!(reader.feed(b"anything").is_err());
    }

    #[test]
    fn v3_projection_selects_one_branch() {
        let h = transcript(SESSION, None).unwrap();
        assert_eq!(h.id, "fixture-session");
        assert_eq!(h.cwd, "/fixture/project");
        assert_eq!(h.timestamp, "2026-11-13T00:00:00.000Z");
        assert_eq!(h.partial, None);
        assert_eq!(h.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["user", "assistant", "tool", "metadata", "final"]);
        assert_eq!(h.entries[0].timestamp, "2026-11-13T00:00:01.000Z");
        let branch = transcript(SESSION, Some("sibling")).unwrap();
        assert_eq!(branch.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["user", "sibling"]);
    }

    #[test]
    fn v3_content_mapping_and_metadata() {
        let h = transcript(SESSION, None).unwrap();
        let m = h.entries[1].message.as_ref().unwrap();
        assert_eq!(m.role, "assistant");
        assert_eq!(m.content, vec![Block::Thinking("Inspect fixture".into()), Block::ToolUse {
            id: "tool-1".into(), name: "read_file".into(), input: json!({"path":"fixture.txt"}),
        }]);
        assert_eq!(h.entries[2].message.as_ref().unwrap().content, vec![Block::ToolResult {
            tool_use_id: "tool-1".into(), content: json!([{"type":"text","text":"fixture content"}]), is_error: Some(false),
        }]);
        assert_eq!(h.entries[3].kind, "custom");
        assert!(h.entries[3].message.is_none());
        assert_eq!(h.entries[4].message.as_ref().unwrap().content, vec![Block::Text("Done".into())]);
        assert_eq!(block(&json!({"type":"image","source":{"url":"https://invalid"}})).unwrap(), Block::Unsupported("image".into()));
        assert!(block(&json!({"type":"tool_use","id":"x","name":"read"})).is_err());
    }

    #[test]
    fn v3_cycle_returns_partial_without_repeating_entries() {
        let h = transcript(&tree(&[("a", Some("b")), ("b", Some("a"))]), None).unwrap();
        assert!(h.partial.unwrap().contains("cycle"));
        assert_eq!(h.entries.len(), 2);
    }

    #[test]
    fn v3_missing_parent_or_leaf_returns_partial() {
        let bytes = tree(&[("a", Some("missing")), ("b", Some("a"))]);
        let h = transcript(&bytes, None).unwrap();
        assert!(h.partial.unwrap().contains("missing"));
        assert_eq!(h.entries.len(), 2);
        let h = transcript(&bytes, Some("absent")).unwrap();
        assert!(h.partial.is_some());
        assert!(h.entries.is_empty());
    }

    #[test]
    fn v3_incomplete_tail_preserves_committed_branch() {
        let bytes = [SESSION, b"{\"type\":\"message\",\"id\":"].concat();
        let h = transcript(&bytes, None).unwrap();
        assert!(h.partial.unwrap().contains("malformed JSON"));
        assert_eq!(h.entries.last().unwrap().id, "final");
    }

    #[test]
    fn v3_duplicate_ids_are_partial() {
        let h = transcript(&tree(&[("a", None), ("a", None)]), None).unwrap();
        assert!(h.partial.unwrap().contains("duplicate"));
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn unsupported_transcript_version_is_explicit() {
        for version in [1, 2, 4] {
            let bytes = json!({"type":"session","version":version,"id":"s","timestamp":"t","cwd":"/fixture"}).to_string();
            assert!(transcript(bytes.as_bytes(), None).unwrap_err().contains("format belum didukung"));
        }
        assert!(transcript(b"", None).is_err());
    }
}
