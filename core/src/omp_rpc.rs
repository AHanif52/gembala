//! Pure contracts for `omp --mode rpc` (FR-110 driver for Oh My Pi), protocol v1/v2.
//!
//! Verified against omp 18.18.9 on 17 Nov 2026; fixtures in `tests/fixtures/omp_rpc`.
//! Commands are JSON lines on stdin and each carries a caller `id`. Stdout carries one
//! `ready` line, then `response` lines matched by `id`, `extension_ui_request` lines, and
//! session event frames. The runner starts using these contracts in phase 2.
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

/// A decoded line is bounded so one runaway frame cannot exhaust the daemon.
pub const LINE_MAX: usize = 8 * 1024 * 1024;
/// Protocol version this driver speaks; `ready.supportedProtocolVersions` must contain it.
pub const PROTOCOL: u64 = 1;
/// Frames over `maxFrameBytes` arrive as base64 `rpc_chunk` parts and are reassembled first.
const CHUNK_PARTS_MAX: usize = 64;

#[derive(Debug, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

#[derive(Debug, PartialEq)]
pub struct Ready {
    pub protocol: u64,
    pub max_frame_bytes: u64,
}

#[derive(Debug, PartialEq)]
pub struct Context {
    pub tokens: u64,
    pub window: u64,
}

/// `get_state` data. The pane status line reads it directly, and a field omp omits stays
/// `None` so the UI can show "—" instead of inventing a value.
#[derive(Debug, Default, PartialEq)]
pub struct State {
    pub session_id: String,
    pub session_file: Option<String>,
    /// `provider/id` as the CLI reports it.
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub context: Option<Context>,
    pub settled: bool,
    pub streaming: bool,
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    Ready(Ready),
    Response { id: String, command: String, ok: bool, data: Value, error: Option<String> },
    /// A `select` request needs an answer carrying the same `id`; anything else is informational.
    UiRequest { id: String, method: String, title: Option<String>, options: Vec<String> },
    ThinkingLevel(String),
    AgentStart,
    AgentEnd,
    TurnStart,
    TurnEnd,
    MessageStart { role: String },
    Delta(Delta),
    MessageEnd(MessageEnd),
    ToolStart { id: String, name: String, args: Value },
    ToolEnd { id: String, name: String, is_error: bool, text: String, details: Value },
    PromptResult { id: String, status: String },
    SessionSettled,
    /// Known-but-inert frames (widgets, command catalog, cost notices) and unknown types.
    Other(String),
}

#[derive(Debug, PartialEq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    ToolCallStart,
    ToolCallEnd { id: String, name: String, args: Value },
}

#[derive(Debug, PartialEq)]
pub struct MessageEnd {
    pub role: String,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    pub usage: Option<Usage>,
    pub text: String,
}

fn str_of(v: &Value, key: &str) -> Result<String, String> {
    v[key].as_str().map(str::to_owned).ok_or_else(|| format!("missing or invalid {key}"))
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn usage_of(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    Some(Usage {
        input: u64_of(u, "input"),
        output: u64_of(u, "output"),
        cache_read: u64_of(u, "cacheRead"),
        cache_write: u64_of(u, "cacheWrite"),
        total: u64_of(u, "totalTokens"),
    })
}

/// Concatenated `text` blocks of a message or tool result. omp truncates long tool output
/// before it reaches the wire and appends its own marker, so this text is all the pane can
/// show; the marker is kept rather than hidden.
fn text_of(v: &Value) -> String {
    v.as_array().into_iter().flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>().join("")
}

fn object(line: &[u8]) -> Result<Value, String> {
    if line.len() > LINE_MAX {
        return Err("omp RPC line exceeds 8 MiB".into());
    }
    let text = std::str::from_utf8(line).map_err(|e| format!("invalid UTF-8: {e}"))?;
    let v: Value = serde_json::from_str(text).map_err(|e| format!("malformed JSON: {e}"))?;
    if !v.is_object() {
        return Err("expected JSON object".into());
    }
    Ok(v)
}

pub fn state(data: &Value) -> Result<State, String> {
    let model = data.get("model").and_then(|m| {
        let id = m["id"].as_str()?;
        let provider = m["provider"].as_str().unwrap_or("");
        Some(if provider.is_empty() { id.to_owned() } else { format!("{provider}/{id}") })
    });
    let context = data.get("contextUsage").and_then(|c| {
        Some(Context { tokens: c["tokens"].as_u64()?, window: c["contextWindow"].as_u64()? })
    });
    Ok(State {
        session_id: str_of(data, "sessionId")?,
        session_file: opt_str(data, "sessionFile"),
        model,
        thinking: opt_str(data, "thinkingLevel"),
        context,
        settled: data.get("isSettled").and_then(Value::as_bool).unwrap_or(true),
        streaming: data.get("isStreaming").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// One stdin command line, without the trailing newline.
pub fn command(id: &str, ty: &str, mut params: Value) -> String {
    params["id"] = json!(id);
    params["type"] = json!(ty);
    params.to_string()
}

pub fn prompt(id: &str, message: &str) -> String {
    command(id, "prompt", json!({ "message": message }))
}

/// `behavior` is `steer` or `followUp`; without it a busy session refuses the prompt.
pub fn prompt_while_streaming(id: &str, message: &str, behavior: &str) -> String {
    command(id, "prompt", json!({ "message": message, "streamingBehavior": behavior }))
}

pub fn abort(id: &str) -> String {
    command(id, "abort", json!({}))
}

pub fn get_state(id: &str) -> String {
    command(id, "get_state", json!({}))
}

pub fn set_model(id: &str, provider: &str, model_id: &str) -> String {
    command(id, "set_model", json!({ "provider": provider, "modelId": model_id }))
}

pub fn set_thinking_level(id: &str, level: &str) -> String {
    command(id, "set_thinking_level", json!({ "level": level }))
}

pub fn new_session(id: &str) -> String {
    command(id, "new_session", json!({}))
}

/// Resumes a saved session. The RPC field is `sessionPath` (a file path), not a session id:
/// resuming by id is the spawn-time form, `omp --resume <id>`.
pub fn switch_session(id: &str, session_path: &str) -> String {
    command(id, "switch_session", json!({ "sessionPath": session_path }))
}

pub fn get_messages(id: &str) -> String {
    command(id, "get_messages", json!({}))
}

/// Answers an `extension_ui_request`. The `id` must be echoed: without it omp treats the
/// client as gone and fails the tool with "RPC client disconnected".
pub fn ui_response(request_id: &str, value: &str) -> String {
    json!({ "type": "extension_ui_response", "id": request_id, "value": value }).to_string()
}

/// Decodes one reassembled stdout line. Unknown frame types stay as `Other` so an omp
/// upgrade that adds frames does not break the pane; a malformed known shape is an error.
pub fn frame(line: &[u8]) -> Result<Frame, String> {
    let v = object(line)?;
    let ty = str_of(&v, "type")?;
    Ok(match ty.as_str() {
        "ready" => Frame::Ready(Ready {
            protocol: v.get("protocolVersion").and_then(Value::as_u64).ok_or("missing protocolVersion")?,
            max_frame_bytes: u64_of(&v, "maxFrameBytes"),
        }),
        "response" => Frame::Response {
            id: str_of(&v, "id")?,
            command: str_of(&v, "command")?,
            ok: v.get("success").and_then(Value::as_bool).ok_or("missing success")?,
            data: v.get("data").cloned().unwrap_or(Value::Null),
            error: opt_str(&v, "error"),
        },
        "extension_ui_request" => {
            let method = str_of(&v, "method")?;
            if method == "select" {
                let options = v["options"].as_array().ok_or("missing options")?
                    .iter().map(|o| o.as_str().map(str::to_owned).ok_or("invalid option"))
                    .collect::<Result<_, _>>()?;
                Frame::UiRequest { id: str_of(&v, "id")?, method, title: opt_str(&v, "title"), options }
            } else {
                Frame::Other(format!("extension_ui_request:{method}"))
            }
        }
        "thinking_level_changed" => Frame::ThinkingLevel(str_of(&v, "thinkingLevel")?),
        "agent_start" => Frame::AgentStart,
        "agent_end" => Frame::AgentEnd,
        "turn_start" => Frame::TurnStart,
        "turn_end" => Frame::TurnEnd,
        "message_start" => Frame::MessageStart { role: str_of(&v["message"], "role")? },
        "message_update" => match update(&v["assistantMessageEvent"])? {
            Some(d) => Frame::Delta(d),
            // text_end/thinking_end only repeat content the deltas already delivered.
            None => Frame::Other("message_update".into()),
        },
        "message_end" => {
            let m = &v["message"];
            Frame::MessageEnd(MessageEnd {
                role: str_of(m, "role")?,
                stop_reason: opt_str(m, "stopReason"),
                error: opt_str(m, "errorMessage"),
                usage: usage_of(m),
                text: text_of(&m["content"]),
            })
        }
        "tool_execution_start" => Frame::ToolStart {
            id: str_of(&v, "toolCallId")?,
            name: str_of(&v, "toolName")?,
            args: v.get("args").cloned().unwrap_or(Value::Null),
        },
        "tool_execution_end" => Frame::ToolEnd {
            id: str_of(&v, "toolCallId")?,
            name: str_of(&v, "toolName")?,
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
            text: text_of(&v["result"]["content"]),
            details: v["result"]["details"].clone(),
        },
        "prompt_result" => Frame::PromptResult { id: str_of(&v, "id")?, status: str_of(&v, "status")? },
        "session_settled" => Frame::SessionSettled,
        other => Frame::Other(other.chars().take(128).collect()),
    })
}

fn update(ev: &Value) -> Result<Option<Delta>, String> {
    Ok(Some(match str_of(ev, "type")?.as_str() {
        "text_delta" => Delta::Text(str_of(ev, "delta")?),
        "thinking_delta" => Delta::Thinking(str_of(ev, "delta")?),
        "toolcall_start" => Delta::ToolCallStart,
        "toolcall_end" => {
            let call = &ev["toolCall"];
            Delta::ToolCallEnd {
                id: str_of(call, "id")?,
                name: str_of(call, "name")?,
                args: call.get("arguments").cloned().unwrap_or(Value::Null),
            }
        }
        _ => return Ok(None),
    }))
}

/// Buffers stdout bytes until a complete line, so a UTF-8 character may span reads, and
/// reassembles `rpc_chunk` sequences into the frame they were split from.
#[derive(Default)]
pub struct Reader {
    pending: Vec<u8>,
    failure: Option<String>,
    chunks: (String, Vec<Option<Vec<u8>>>),
}

impl Reader {
    /// Feeds bytes and returns every frame completed so far. A chunk sequence yields only
    /// its final reassembled frame.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Frame>, String> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let n = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len());
            if self.pending.len() + n > LINE_MAX {
                return self.fail("omp RPC line exceeds 8 MiB".into());
            }
            self.pending.extend_from_slice(&bytes[..n]);
            if n == bytes.len() {
                break;
            }
            let mut line = std::mem::take(&mut self.pending);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if !line.is_empty() {
                match self.line(&line) {
                    Ok(Some(f)) => out.push(f),
                    Ok(None) => {}
                    Err(e) => return self.fail(e),
                }
            }
            bytes = &bytes[n + 1..];
        }
        Ok(out)
    }

    fn line(&mut self, line: &[u8]) -> Result<Option<Frame>, String> {
        let v = object(line)?;
        if v["type"] != "rpc_chunk" {
            return frame(line).map(Some);
        }
        let chunk_id = str_of(&v, "chunkId")?;
        let index = v["index"].as_u64().ok_or("invalid chunk index")? as usize;
        let count = v["count"].as_u64().ok_or("invalid chunk count")? as usize;
        if count == 0 || count > CHUNK_PARTS_MAX || index >= count {
            return Err("invalid rpc_chunk bounds".into());
        }
        if self.chunks.0 != chunk_id {
            self.chunks = (chunk_id.clone(), vec![None; count]);
        }
        let parts = &mut self.chunks.1;
        if parts.len() != count {
            return Err("conflicting rpc_chunk count".into());
        }
        parts[index] = Some(B64.decode(str_of(&v, "data")?)
            .map_err(|e| format!("invalid rpc_chunk data: {e}"))?);
        if parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let joined: Vec<u8> = parts.iter().flatten().flatten().copied().collect();
        self.chunks = (String::new(), Vec::new());
        frame(&joined).map(Some)
    }

    fn fail<T>(&mut self, e: String) -> Result<T, String> {
        self.pending.clear();
        self.chunks = (String::new(), Vec::new());
        self.failure = Some(e.clone());
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN: &[u8] = include_bytes!("../tests/fixtures/omp_rpc/turn.ndjson");
    const ABORT: &[u8] = include_bytes!("../tests/fixtures/omp_rpc/abort.ndjson");
    const DIFF: &[u8] = include_bytes!("../tests/fixtures/omp_rpc/diff.ndjson");

    fn read_all(bytes: &[u8]) -> Result<Vec<Frame>, String> {
        let mut reader = Reader::default();
        let mut out = reader.feed(bytes)?;
        if !reader.pending.is_empty() {
            let line = std::mem::take(&mut reader.pending);
            if let Some(f) = reader.line(&line)? {
                out.push(f);
            }
        }
        Ok(out)
    }

    #[test]
    fn turn_fixture_decodes_handshake_stream_and_text() {
        let frames = read_all(TURN).unwrap();
        assert_eq!(frames[0], Frame::Ready(Ready { protocol: 1, max_frame_bytes: 1048576 }));
        assert!(frames.contains(&Frame::Delta(Delta::Text("Selesai ".into()))));
        assert!(frames.contains(&Frame::Delta(Delta::Text("🌱".into()))));
        let thinking = frames.iter().filter_map(|f| match f {
            Frame::Delta(Delta::Thinking(d)) => Some(d.as_str()),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(thinking, ["Menulis ", "berkas"]);
        assert!(frames.contains(&Frame::PromptResult { id: "p1".into(), status: "completed".into() }));
        assert!(frames.contains(&Frame::SessionSettled));
        assert!(frames.contains(&Frame::Other("available_commands_update".into())));
    }

    #[test]
    fn approval_request_decodes_and_the_answer_echoes_its_id() {
        let frames = read_all(TURN).unwrap();
        let (id, options) = frames.iter().find_map(|f| match f {
            Frame::UiRequest { id, method, options, .. } if method == "select" => Some((id.clone(), options.clone())),
            _ => None,
        }).unwrap();
        assert_eq!(id, "ui-2");
        assert_eq!(options, ["Approve", "Approve write for this session", "Deny"]);
        let reply: Value = serde_json::from_str(&ui_response(&id, "Approve")).unwrap();
        assert_eq!(reply, json!({ "type": "extension_ui_response", "id": "ui-2", "value": "Approve" }));
    }

    #[test]
    fn widget_requests_are_inert_not_approvals() {
        let frames = read_all(TURN).unwrap();
        assert!(frames.contains(&Frame::Other("extension_ui_request:setWidget".into())));
        assert_eq!(frames.iter().filter(|f| matches!(f, Frame::UiRequest { .. })).count(), 1);
    }

    #[test]
    fn tool_call_is_reassembled_from_start_and_end() {
        let frames = read_all(TURN).unwrap();
        let args = json!({ "path": "halo.txt", "content": "halo\n" });
        assert!(frames.contains(&Frame::Delta(Delta::ToolCallStart)));
        assert!(frames.contains(&Frame::Delta(Delta::ToolCallEnd { id: "call-1".into(), name: "write".into(), args: args.clone() })));
        assert!(frames.contains(&Frame::ToolStart { id: "call-1".into(), name: "write".into(), args }));
        assert!(frames.contains(&Frame::ToolEnd {
            id: "call-1".into(), name: "write".into(), is_error: false,
            text: "Successfully wrote 5 bytes to halo.txt".into(),
            details: json!({ "resolvedPath": "/fixture/project/halo.txt" }),
        }));
    }

    #[test]
    fn edit_result_carries_a_structured_diff() {
        let frames = read_all(DIFF).unwrap();
        let details = frames.iter().find_map(|f| match f {
            Frame::ToolEnd { name, details, .. } if name == "edit" => Some(details.clone()),
            _ => None,
        }).unwrap();
        assert_eq!(details["op"], "update");
        assert_eq!(details["firstChangedLine"], 2);
        assert_eq!(details["path"], "catatan.txt");
        assert_eq!(details["oldText"], "baris1\nbaris2\n");
        assert_eq!(details["newText"], "baris1\nbaris2 diganti\n");
    }

    #[test]
    fn assistant_usage_and_text_come_from_message_end() {
        let frames = read_all(TURN).unwrap();
        let end = frames.iter().find_map(|f| match f {
            Frame::MessageEnd(m) if m.role == "assistant" && m.stop_reason.as_deref() == Some("stop") => Some(m),
            _ => None,
        }).unwrap();
        assert_eq!(end.text, "Selesai 🌱");
        assert_eq!(end.usage, Some(Usage { input: 167, output: 28, cache_read: 33408, cache_write: 0, total: 33603 }));
        assert_eq!(end.error, None);
    }

    #[test]
    fn abort_is_its_own_stop_reason() {
        let frames = read_all(ABORT).unwrap();
        let end = frames.iter().find_map(|f| match f {
            Frame::MessageEnd(m) if m.role == "assistant" => Some(m),
            _ => None,
        }).unwrap();
        assert_eq!(end.stop_reason.as_deref(), Some("aborted"));
        assert_eq!(end.error.as_deref(), Some("Interrupted by user"));
        assert!(frames.contains(&Frame::PromptResult { id: "p1".into(), status: "aborted".into() }));
    }

    #[test]
    fn state_reports_model_thinking_and_context_or_leaves_them_empty() {
        let frames = read_all(TURN).unwrap();
        let data = frames.iter().find_map(|f| match f {
            Frame::Response { command, ok: true, data, .. } if command == "get_state" => Some(data.clone()),
            _ => None,
        }).unwrap();
        let s = state(&data).unwrap();
        assert_eq!(s.session_id, "fixture-session");
        assert_eq!(s.session_file.as_deref(), Some("/fixture/project/sessions/fixture-session.jsonl"));
        assert_eq!(s.model.as_deref(), Some("fixture/model-a"));
        assert_eq!(s.thinking.as_deref(), Some("low"));
        assert_eq!(s.context, Some(Context { tokens: 1000, window: 200000 }));
        assert!(s.settled && !s.streaming);
        assert!(state(&json!({})).is_err());
        let bare = state(&json!({ "sessionId": "s" })).unwrap();
        assert_eq!((bare.model, bare.context, bare.thinking), (None, None, None));
    }

    #[test]
    fn failed_response_keeps_the_error_text() {
        let f = frame(br#"{"id":"a","type":"response","command":"set_model","success":false,"error":"Model not found: x"}"#).unwrap();
        assert_eq!(f, Frame::Response {
            id: "a".into(), command: "set_model".into(), ok: false, data: Value::Null,
            error: Some("Model not found: x".into()),
        });
    }

    #[test]
    fn commands_are_single_lines_with_id_and_type() {
        let v: Value = serde_json::from_str(&prompt("p9", "halo\nbaris dua")).unwrap();
        assert_eq!(v, json!({ "id": "p9", "type": "prompt", "message": "halo\nbaris dua" }));
        assert!(!prompt("p9", "halo\nbaris dua").contains('\n'));
        let v: Value = serde_json::from_str(&switch_session("s1", "/x/y.jsonl")).unwrap();
        assert_eq!(v["sessionPath"], "/x/y.jsonl");
        assert_eq!(v.get("paths"), None);
        let v: Value = serde_json::from_str(&prompt_while_streaming("p2", "tambah", "steer")).unwrap();
        assert_eq!(v["streamingBehavior"], "steer");
        let v: Value = serde_json::from_str(&set_model("m", "fixture", "model-a")).unwrap();
        assert_eq!((v["provider"].clone(), v["modelId"].clone()), (json!("fixture"), json!("model-a")));
    }

    #[test]
    fn unknown_frames_stay_inert() {
        let frames = read_all(b"{\"type\":\"future_frame\",\"x\":1}\n").unwrap();
        assert_eq!(frames, vec![Frame::Other("future_frame".into())]);
    }

    #[test]
    fn malformed_known_shapes_are_errors() {
        for line in ["omp text", "{", "[]", "{\"type\":\"response\",\"id\":\"a\"}",
            "{\"type\":\"extension_ui_request\",\"id\":\"a\",\"method\":\"select\"}",
            "{\"type\":\"ready\"}",
            "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\"}}"] {
            assert!(frame(line.as_bytes()).is_err(), "accepted {line}");
        }
        assert!(frame(b"{\"type\":\"message_end\",\"message\":{}}").is_err());
    }

    #[test]
    fn byte_by_byte_and_crlf_match_whole_input() {
        let expected = read_all(TURN).unwrap();
        let mut reader = Reader::default();
        let mut actual = Vec::new();
        for byte in TURN {
            actual.extend(reader.feed(&[*byte]).unwrap());
        }
        assert_eq!(actual, expected);
        let crlf = String::from_utf8(TURN.to_vec()).unwrap().replace('\n', "\r\n");
        assert_eq!(read_all(crlf.as_bytes()).unwrap(), expected);
    }

    #[test]
    fn oversized_frames_are_reassembled_from_chunks() {
        let inner = json!({ "type": "notice", "level": "info", "message": "x".repeat(50) }).to_string();
        let encoded = B64.encode(inner.as_bytes());
        let (a, b) = encoded.split_at(20);
        let mut reader = Reader::default();
        let first = json!({ "type": "rpc_chunk", "chunkId": "c1", "index": 0, "count": 2, "byteLength": encoded.len(), "data": a });
        assert!(reader.feed(format!("{first}\n").as_bytes()).unwrap().is_empty());
        let second = json!({ "type": "rpc_chunk", "chunkId": "c1", "index": 1, "count": 2, "byteLength": encoded.len(), "data": b });
        let frames = reader.feed(format!("{second}\n").as_bytes()).unwrap();
        assert_eq!(frames, vec![Frame::Other("notice".into())]);
    }

    #[test]
    fn broken_chunk_bounds_are_errors() {
        for line in [
            "{\"type\":\"rpc_chunk\",\"chunkId\":\"c\",\"index\":5,\"count\":2,\"data\":\"\"}",
            "{\"type\":\"rpc_chunk\",\"chunkId\":\"c\",\"index\":0,\"count\":0,\"data\":\"\"}",
            "{\"type\":\"rpc_chunk\",\"chunkId\":\"c\",\"index\":0,\"count\":9999,\"data\":\"\"}",
            "{\"type\":\"rpc_chunk\",\"chunkId\":\"c\",\"index\":0,\"count\":1,\"data\":\"!!!\"}",
        ] {
            assert!(read_all(line.as_bytes()).is_err(), "accepted {line}");
        }
    }

    #[test]
    fn line_limit_is_explicit_and_failure_is_sticky() {
        let mut reader = Reader::default();
        reader.feed(&vec![b' '; LINE_MAX]).unwrap();
        assert!(reader.feed(b"x").unwrap_err().contains("8 MiB"));
        assert!(reader.feed(b"\n").unwrap_err().contains("8 MiB"));
    }
}
