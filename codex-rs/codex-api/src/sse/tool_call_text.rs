//! Recovery parser for tool calls that a model leaked into assistant text.
//!
//! Models served via Chat Completions gateways (e.g. Hoonify/DeepSeek) that are
//! not prompted with their native chat template sometimes emit tool calls as
//! free text in the `content` field instead of the structured `tool_calls`
//! array. They typically improvise Anthropic's XML shape:
//!
//! ```text
//! <invoke name="shell">
//! <parameter name="cmd" string="true">ls -la</parameter>
//! <parameter name="workdir">/repo</parameter>
//! </invoke>
//! ```
//!
//! or a Hermes/Qwen-style JSON block:
//!
//! ```text
//! <tool_call>{"name": "shell", "arguments": {"cmd": "ls -la"}}</tool_call>
//! ```
//!
//! These never reach the dispatcher because nothing structured exists. This
//! module scrapes them back out so the turn can proceed; the recovered names
//! and arguments are then normalized by the core tool-compatibility shim.

/// A tool call recovered from assistant text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedToolCall {
    pub name: String,
    /// JSON object string, matching the `arguments` field of a structured
    /// function call.
    pub arguments: String,
}

/// Markers that indicate the assistant text contains a leaked tool call.
const TOOL_CALL_MARKERS: [&str; 4] =
    ["<invoke", "<function_calls", "<tool_calls", "<tool_call>"];

/// Extract every leaked tool call from `text`. Returns an empty vec when the
/// text contains no recognizable tool-call markup.
pub(crate) fn extract_text_tool_calls(text: &str) -> Vec<ExtractedToolCall> {
    let mut out = Vec::new();
    extract_invoke_blocks(text, &mut out);
    if out.is_empty() {
        extract_tool_call_json_blocks(text, &mut out);
    }
    out
}

/// Byte offset of the first tool-call marker, so the caller can keep the prose
/// the model wrote *before* it started leaking calls.
pub(crate) fn first_tool_call_marker(text: &str) -> Option<usize> {
    TOOL_CALL_MARKERS
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
}

fn extract_invoke_blocks(text: &str, out: &mut Vec<ExtractedToolCall>) {
    let mut rest = text;
    while let Some(pos) = rest.find("<invoke") {
        let after = &rest[pos + "<invoke".len()..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let attrs = &after[..tag_end];
        let body = &after[tag_end + 1..];

        let Some(name) = attr_value(attrs, "name") else {
            rest = body;
            continue;
        };

        // The block ends at its own close tag or the start of the next invoke,
        // whichever comes first — tolerating a missing `</invoke>`.
        let block_end = body
            .find("</invoke>")
            .into_iter()
            .chain(body.find("<invoke"))
            .min()
            .unwrap_or(body.len());

        out.push(ExtractedToolCall {
            name,
            arguments: parse_parameters(&body[..block_end]),
        });
        rest = &body[block_end..];
    }
}

fn parse_parameters(block: &str) -> String {
    let mut map = serde_json::Map::new();
    let mut rest = block;
    while let Some(pos) = rest.find("<parameter") {
        let after = &rest[pos + "<parameter".len()..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let attrs = &after[..tag_end];
        let value_start = &after[tag_end + 1..];

        let Some(key) = attr_value(attrs, "name") else {
            rest = value_start;
            continue;
        };

        let (value, next) = match value_start.find("</parameter>") {
            Some(end) => (
                &value_start[..end],
                &value_start[end + "</parameter>".len()..],
            ),
            None => (value_start, ""),
        };

        map.insert(key, serde_json::Value::String(value.trim().to_string()));
        if next.is_empty() {
            break;
        }
        rest = next;
    }
    serde_json::Value::Object(map).to_string()
}

fn extract_tool_call_json_blocks(text: &str, out: &mut Vec<ExtractedToolCall>) {
    let mut rest = text;
    while let Some(pos) = rest.find("<tool_call>") {
        let after = &rest[pos + "<tool_call>".len()..];
        let (body, next) = match after.find("</tool_call>") {
            Some(end) => (&after[..end], &after[end + "</tool_call>".len()..]),
            None => (after, ""),
        };

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim())
            && let Some(name) = value.get("name").and_then(serde_json::Value::as_str)
        {
            let arguments = match value.get("arguments").or_else(|| value.get("parameters")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "{}".to_string(),
            };
            out.push(ExtractedToolCall {
                name: name.to_string(),
                arguments,
            });
        }

        if next.is_empty() {
            break;
        }
        rest = next;
    }
}

/// Read the value of an HTML-ish attribute (`key="value"` / `key='value'`),
/// tolerating extra attributes around it. Returns `None` when the key is
/// absent or unquoted.
fn attr_value(attrs: &str, key: &str) -> Option<String> {
    let mut search = attrs;
    loop {
        let pos = search.find(key)?;
        let after = search[pos + key.len()..].trim_start();
        if let Some(rest) = after.strip_prefix('=') {
            let rest = rest.trim_start();
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let value = &rest[quote.len_utf8()..];
                if let Some(end) = value.find(quote) {
                    return Some(value[..end].to_string());
                }
            }
            return None;
        }
        // `key` was a substring of some other attribute name; keep scanning.
        search = &search[pos + key.len()..];
    }
}

#[cfg(test)]
#[path = "tool_call_text_tests.rs"]
mod tool_call_text_tests;
