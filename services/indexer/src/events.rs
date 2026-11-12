//! Parse NEP-297 neap events from NEAR block receipts.

use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

const PREFIX: &str = "EVENT_JSON:";

#[derive(Debug, Clone)]
pub struct NeapEvent {
    pub event: String,
    pub data: Value,
}

/// Extract all neap events from a list of receipt log lines.
pub fn parse_logs(logs: &[String]) -> Vec<NeapEvent> {
    let mut out = Vec::new();
    for line in logs {
        let Some(json_str) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<EventEnvelope>(json_str) else {
            warn!(line, "bad event JSON");
            continue;
        };
        if envelope.standard != "neap" {
            continue;
        }
        for d in envelope.data {
            out.push(NeapEvent {
                event: envelope.event.clone(),
                data: d,
            });
        }
    }
    out
}

#[derive(Deserialize)]
struct EventEnvelope {
    standard: String,
    #[allow(dead_code)]
    version: String,
    event: String,
    data: Vec<Value>,
}
