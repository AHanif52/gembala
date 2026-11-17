//! Antigravity stream-json driver. It normalizes CLI events to the structured-pane contract.

use crate::commandcode::{Envelope, RunResult, Usage};
use serde_json::{Value, json};

const LINE_MAX: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub struct Reader {
    pending: Vec<u8>,
    result_seen: bool,
    closed: bool,
    failure: Option<String>,
}

impl Reader {
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Envelope>, String> {
        if let Some(error) = &self.failure { return Err(error.clone()); }
        if self.closed { return Err("stream already finished".into()); }
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let n = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len());
            if self.pending.len() + n > LINE_MAX { return self.fail("Antigravity line exceeds 8 MiB".into()); }
            self.pending.extend_from_slice(&bytes[..n]);
            if n == bytes.len() { break; }
            match self.line() {
                Ok(Some(event)) => out.push(event),
                Ok(None) => {}
                Err(error) => return self.fail(error),
            }
            bytes = &bytes[n + 1..];
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Result<Vec<Envelope>, String> {
        let mut out = self.feed(&[])?;
        if !self.pending.is_empty() {
            match self.line() {
                Ok(Some(event)) => out.push(event),
                Ok(None) => {}
                Err(error) => return self.fail(error),
            }
        }
        if !self.result_seen { return self.fail("EOF without result".into()); }
        self.closed = true;
        Ok(out)
    }

    fn fail<T>(&mut self, error: String) -> Result<T, String> {
        self.pending.clear();
        self.failure = Some(error.clone());
        Err(error)
    }

    fn line(&mut self) -> Result<Option<Envelope>, String> {
        let bytes = std::mem::take(&mut self.pending);
        if bytes.iter().all(u8::is_ascii_whitespace) { return Ok(None); }
        if self.result_seen { return Err("line after final result".into()); }
        let text = std::str::from_utf8(&bytes).map_err(|e| format!("invalid UTF-8: {e}"))?;
        let line: Value = serde_json::from_str(text).map_err(|e| format!("malformed JSON: {e}"))?;
        if !line.is_object() { return Err("expected JSON object".into()); }
        let event = line["event"].as_str().unwrap_or_default();
        match event {
            "init" => {
                let mut event = json!({"type":"run_start"});
                if let Some(id) = line["conversation_id"].as_str().filter(|s| !s.is_empty()) { event["sessionId"] = json!(id); }
                if let Some(model) = line.pointer("/init/model").and_then(Value::as_str) { event["model"] = json!(model); }
                Ok(Some(Envelope::Event(event)))
            }
            "step_update" => Ok(step(&line["step_update"])),
            "result" => {
                let result = &line["result"];
                let status = result["status"].as_str().unwrap_or("ERROR");
                if !matches!(status, "SUCCESS" | "ERROR" | "CANCELED" | "INTERRUPTED" | "WAITING" | "RUNNING" | "INVALID") {
                    return Err("invalid Antigravity result status".into());
                }
                let usage = &result["usage"];
                let number = |key: &str| usage[key].as_u64();
                let duration_ms = result["duration_seconds"].as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| (n * 1000.0) as u64);
                let run = RunResult {
                    subtype: match status { "SUCCESS" => "success", "CANCELED" | "INTERRUPTED" => "interrupted", _ => "error" }.into(),
                    session_id: result["conversation_id"].as_str().map(str::to_string),
                    stop_reason: None,
                    usage: Usage { input_tokens:number("input_tokens"), output_tokens:number("output_tokens"), cache_read_tokens:number("cache_read_tokens"), cache_write_tokens:None },
                    duration_ms,
                    final_text: result["response"].as_str().map(str::to_string),
                    error: result.get("error").cloned(),
                };
                self.result_seen = true;
                Ok(Some(Envelope::Result(run)))
            }
            "error" => Ok(Some(Envelope::Event(json!({"type":"run_error", "error":line.get("error").or_else(|| line.get("message")).cloned().unwrap_or(Value::Null)})))),
            _ => Ok(None),
        }
    }
}

fn step(update: &Value) -> Option<Envelope> {
    let kind = update["step_type"].as_str()?;
    let state = update["state"].as_str().unwrap_or_default();
    if kind == "agent_response" {
        let delta = update["text_delta"].as_str().unwrap_or_default();
        return (!delta.is_empty()).then(|| Envelope::Event(json!({"type":"text_delta", "delta":delta})));
    }
    if kind != "tool" { return None; }
    let info = &update["tool_info"];
    let name = update["tool_name"].as_str().or(info["name"].as_str()).unwrap_or("unknown");
    let id = format!("agy:{}:{}", update["conversation_id"].as_str().unwrap_or("session"), update["step_index"].as_u64().unwrap_or(0));
    let params = info.get("parameters").cloned().unwrap_or(Value::Null);
    let (kind, extra) = match state {
        "ACTIVE" => ("tool_running", json!({"description": update.get("description")})),
        "DONE" => ("tool_completed", json!({"result":info.get("output")})),
        "ERROR" | "FAILED" | "CANCELED" | "CANCELLED" => ("tool_errored", json!({"error":info.get("error").or(info.get("output"))})),
        _ => ("tool_queued", Value::Null),
    };
    let mut event = json!({"type":kind, "toolCallId":id, "toolName":name, "input":params});
    if let (Some(target), Some(source)) = (event.as_object_mut(), extra.as_object()) { target.extend(source.clone()); }
    Some(Envelope::Event(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &[u8] = include_bytes!("../tests/fixtures/agy/stream.ndjson");

    #[test]
    fn fixture_maps_live_stream_and_two_turn_results() {
        let mut reader = Reader::default();
        let events = reader.feed(STREAM).unwrap();
        let mut events = events;
        events.extend(reader.finish().unwrap());
        assert_eq!(events.iter().filter(|e| matches!(e, Envelope::Result(_))).count(), 1);
        assert!(events.iter().any(|e| matches!(e, Envelope::Event(v) if v["type"] == "run_start" && v["sessionId"] == "agy-fixture-session")));
        assert!(events.iter().any(|e| matches!(e, Envelope::Event(v) if v["type"] == "tool_running" && v["toolName"] == "write_file")));
        assert!(events.iter().any(|e| matches!(e, Envelope::Event(v) if v["type"] == "text_delta" && v["delta"] == "cedar")));
        assert!(events.iter().any(|e| matches!(e, Envelope::Event(v) if v["type"] == "text_delta" && v["delta"] == "\n")));
        assert!(events.iter().any(|e| matches!(e, Envelope::Result(r) if r.usage.input_tokens == Some(11906) && r.final_text.as_deref() == Some("cedar\n"))));
    }

    #[test]
    fn stream_handles_split_lines_and_rejects_missing_result() {
        let mut reader = Reader::default();
        for byte in STREAM { reader.feed(&[*byte]).unwrap(); }
        assert!(reader.finish().is_ok());
        let mut incomplete = Reader::default();
        incomplete.feed(br#"{"event":"init"}"#).unwrap();
        assert_eq!(incomplete.finish().unwrap_err(), "EOF without result");
    }
}
