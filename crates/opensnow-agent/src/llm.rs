//! Minimal Anthropic Messages API client (blocking, via ureq).
//!
//! Used by the schema-refactor planner to reason over the warehouse. Reads
//! `ANTHROPIC_API_KEY` from the environment only after the caller explicitly
//! enables LLM planning with `OPENSNOW_ENABLE_LLM_PLANNER=1`; model defaults to
//! a current Claude and can be overridden with `OPENSNOW_LLM_MODEL`. Everything
//! degrades gracefully — callers check `available()` and fall back to heuristics.

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

fn llm_planner_enabled() -> bool {
    std::env::var("OPENSNOW_ENABLE_LLM_PLANNER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// True when LLM planner calls are explicitly enabled and an API key is configured.
pub fn available() -> bool {
    llm_planner_enabled()
        && std::env::var("ANTHROPIC_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
}

pub fn model() -> String {
    std::env::var("OPENSNOW_LLM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// Single-turn completion. Returns the assistant's text.
pub fn complete(system: &str, user: &str, max_tokens: u32) -> Result<String> {
    if !llm_planner_enabled() {
        return Err(anyhow!(
            "OPENSNOW_ENABLE_LLM_PLANNER is not enabled; refusing external LLM call"
        ));
    }
    let key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| anyhow!("ANTHROPIC_API_KEY not set"))?;
    let base =
        std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build();
    let resp = agent
        .post(&format!("{}/v1/messages", base.trim_end_matches('/')))
        .set("x-api-key", &key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(json!({
            "model": model(),
            "max_tokens": max_tokens,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        }));

    match resp {
        Ok(r) => {
            let v: Value = r.into_json()?;
            v.get("content")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find_map(|b| b.get("text").and_then(Value::as_str)))
                .map(str::to_string)
                .ok_or_else(|| anyhow!("no text in Anthropic response"))
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            Err(anyhow!(
                "anthropic {code}: {}",
                &body[..body.len().min(300)]
            ))
        }
        Err(e) => Err(anyhow!("anthropic request failed: {e}")),
    }
}

/// Pull the first JSON object out of an LLM reply (handles ``` fences / prose).
pub fn extract_json(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
        return Some(v);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        serde_json::from_str::<Value>(&text[start..=end]).ok()
    } else {
        None
    }
}
