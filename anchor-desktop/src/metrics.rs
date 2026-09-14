use std::time::{SystemTime, UNIX_EPOCH};

fn ts_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Emit a structured JSON metrics line to the log.
///
/// The payload must be a `serde_json::Value::Object`. A `"ts_ms"` field
/// (current unix time in milliseconds) is inserted automatically.
pub(crate) fn emit(mut payload: serde_json::Value) {
    if let Some(m) = payload.as_object_mut() {
        m.insert("ts_ms".to_string(), serde_json::json!(ts_ms()));
    }
    log::debug!(target: "anchor::metrics", "[METRICS] {}", payload);
}

/// Emit a pre-serialised JSON string, splicing in a `"ts_ms"` field before the first comma.
pub(crate) fn emit_raw(json: &str) {
    let ts = ts_ms();
    // Insert after the opening `{` so parsers always see ts_ms.
    if let Some(rest) = json.strip_prefix('{') {
        log::debug!(target: "anchor::metrics", "[METRICS] {{\"ts_ms\":{},{}", ts, rest);
    } else {
        log::debug!(target: "anchor::metrics", "[METRICS] {}", json);
    }
}
