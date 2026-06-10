use crate::common::ResponsesApiRequest;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;

const CHAT_COMPLETIONS_TOOL_INSTRUCTIONS: &str = r#"You are running inside the Hoondex CLI through a Chat Completions tool-calling adapter.
The request's `tools` array is the authoritative list of available host actions. When you need to inspect files, run shell commands, edit code, search, or perform any other host action, emit native `tool_calls` using one of those exact tool names and argument schemas.
Do not merely say that you will inspect files, run commands, or make edits. Call the appropriate tool.
Do not invent XML tags, markdown tool blocks, or tool names from other agent harnesses. If a desired helper is absent, choose the closest provided tool or explain the limitation."#;

/// Translate a Responses-API request into a Chat Completions request body.
///
/// Codex builds every turn as a `ResponsesApiRequest`. Providers that only speak the classic
/// OpenAI `/v1/chat/completions` schema (for example Hoonify) need that request reshaped into a
/// `messages` array plus chat-style `tools`. This is the inverse of the SSE translation in
/// `crate::sse::chat`.
pub(crate) fn build_chat_completions_body(request: &ResponsesApiRequest) -> Value {
    let tools = responses_tools_to_chat(&request.tools);

    let mut messages = Vec::<Value>::new();
    if !request.instructions.is_empty() {
        messages.push(json!({"role": "system", "content": request.instructions}));
    }
    if !tools.is_empty() {
        messages.push(json!({"role": "system", "content": CHAT_COMPLETIONS_TOOL_INSTRUCTIONS}));
    }

    let input = request.input.as_slice();

    // Chat Completions has no first-class reasoning item, so fold reasoning text onto the nearest
    // assistant message / tool call that follows the last user turn.
    let mut reasoning_by_anchor_index: HashMap<usize, String> = HashMap::new();
    let mut last_emitted_role: Option<&str> = None;
    for item in input {
        match item {
            ResponseItem::Message { role, .. } => last_emitted_role = Some(chat_message_role(role)),
            ResponseItem::FunctionCall { .. } | ResponseItem::LocalShellCall { .. } => {
                last_emitted_role = Some("assistant")
            }
            ResponseItem::FunctionCallOutput { .. } => last_emitted_role = Some("tool"),
            _ => {}
        }
    }

    let mut last_user_index: Option<usize> = None;
    for (idx, item) in input.iter().enumerate() {
        if let ResponseItem::Message { role, .. } = item
            && role == "user"
        {
            last_user_index = Some(idx);
        }
    }

    if !matches!(last_emitted_role, Some("user")) {
        for (idx, item) in input.iter().enumerate() {
            if let Some(u_idx) = last_user_index
                && idx <= u_idx
            {
                continue;
            }

            if let ResponseItem::Reasoning {
                content: Some(items),
                ..
            } = item
            {
                let mut text = String::new();
                for entry in items {
                    match entry {
                        ReasoningItemContent::ReasoningText { text: segment }
                        | ReasoningItemContent::Text { text: segment } => text.push_str(segment),
                    }
                }
                if text.trim().is_empty() {
                    continue;
                }

                let mut attached = false;
                if idx > 0
                    && let ResponseItem::Message { role, .. } = &input[idx - 1]
                    && role == "assistant"
                {
                    reasoning_by_anchor_index
                        .entry(idx - 1)
                        .and_modify(|v| v.push_str(&text))
                        .or_insert(text.clone());
                    attached = true;
                }

                if !attached && idx + 1 < input.len() {
                    match &input[idx + 1] {
                        ResponseItem::FunctionCall { .. } | ResponseItem::LocalShellCall { .. } => {
                            reasoning_by_anchor_index
                                .entry(idx + 1)
                                .and_modify(|v| v.push_str(&text))
                                .or_insert(text.clone());
                        }
                        ResponseItem::Message { role, .. } if role == "assistant" => {
                            reasoning_by_anchor_index
                                .entry(idx + 1)
                                .and_modify(|v| v.push_str(&text))
                                .or_insert(text.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut last_assistant_text: Option<String> = None;

    for (idx, item) in input.iter().enumerate() {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let mut text = String::new();
                let mut items: Vec<Value> = Vec::new();
                let mut saw_image = false;

                for c in content {
                    match c {
                        ContentItem::InputText { text: t }
                        | ContentItem::OutputText { text: t } => {
                            text.push_str(t);
                            items.push(json!({"type":"text","text": t}));
                        }
                        ContentItem::InputImage { image_url, .. } => {
                            saw_image = true;
                            items.push(json!({"type":"image_url","image_url": {"url": image_url}}));
                        }
                    }
                }

                if role == "assistant" {
                    if let Some(prev) = &last_assistant_text
                        && prev == &text
                    {
                        continue;
                    }
                    last_assistant_text = Some(text.clone());
                }

                let content_value = if role == "assistant" {
                    json!(text)
                } else if saw_image {
                    json!(items)
                } else {
                    json!(text)
                };

                let mut msg = json!({"role": chat_message_role(role), "content": content_value});
                if role == "assistant"
                    && let Some(reasoning) = reasoning_by_anchor_index.get(&idx)
                    && let Some(obj) = msg.as_object_mut()
                {
                    obj.insert("reasoning".to_string(), json!(reasoning));
                }
                messages.push(msg);
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                let reasoning = reasoning_by_anchor_index.get(&idx).map(String::as_str);
                let tool_call = json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    }
                });
                push_tool_call_message(&mut messages, tool_call, reasoning);
            }
            ResponseItem::LocalShellCall {
                id,
                call_id: _,
                status,
                action,
            } => {
                let reasoning = reasoning_by_anchor_index.get(&idx).map(String::as_str);
                let tool_call = json!({
                    "id": id.clone().unwrap_or_default(),
                    "type": "local_shell_call",
                    "status": status,
                    "action": action,
                });
                push_tool_call_message(&mut messages, tool_call, reasoning);
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output_body_to_chat_content(&output.body),
                }));
            }
            ResponseItem::CustomToolCall {
                id,
                call_id: _,
                name,
                input,
                status: _,
            } => {
                let tool_call = json!({
                    "id": id,
                    "type": "custom",
                    "custom": {
                        "name": name,
                        "input": input,
                    }
                });
                let reasoning = reasoning_by_anchor_index.get(&idx).map(String::as_str);
                push_tool_call_message(&mut messages, tool_call, reasoning);
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output_body_to_chat_content(&output.body),
                }));
            }
            // Reasoning is folded in above; the remaining items have no Chat Completions
            // equivalent and are dropped.
            _ => continue,
        }
    }

    let mut payload = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });

    if let Some(obj) = payload.as_object_mut() {
        if !tools.is_empty() {
            obj.insert("tools".to_string(), json!(tools));
            obj.insert("tool_choice".to_string(), json!(request.tool_choice));
            obj.insert(
                "parallel_tool_calls".to_string(),
                json!(request.parallel_tool_calls),
            );
        }
    }

    payload
}

fn chat_message_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        _ => role,
    }
}

fn output_body_to_chat_content(body: &FunctionCallOutputBody) -> Value {
    match body {
        FunctionCallOutputBody::Text(text) => json!(text),
        FunctionCallOutputBody::ContentItems(items) => {
            let mapped: Vec<Value> = items
                .iter()
                .filter_map(|it| match it {
                    FunctionCallOutputContentItem::InputText { text } => {
                        Some(json!({"type":"text","text": text}))
                    }
                    FunctionCallOutputContentItem::InputImage { image_url, .. } => {
                        Some(json!({"type":"image_url","image_url": {"url": image_url}}))
                    }
                    _ => None,
                })
                .collect();
            json!(mapped)
        }
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;

/// Convert Responses-API tool definitions into Chat Completions tool definitions.
///
/// Responses encodes function tools flat (`{"type":"function","name":..,"parameters":..}`),
/// whereas Chat Completions nests them under a `function` object. Namespaced tool groups (for
/// example `multi_agent`) are flattened into their inner function tools. Built-in Responses-only
/// tools that have no Chat Completions equivalent (`web_search`, `image_generation`, custom/
/// freeform tools, ...) are dropped so the request stays schema-valid for plain chat backends.
fn responses_tools_to_chat(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .flat_map(chat_tools_from_responses_tool)
        .collect()
}

fn chat_tools_from_responses_tool(tool: &Value) -> Vec<Value> {
    let Some(obj) = tool.as_object() else {
        return Vec::new();
    };
    // Already in Chat Completions shape: {"type":"function","function":{...}}.
    if obj.contains_key("function") {
        return vec![tool.clone()];
    }
    match obj.get("type").and_then(Value::as_str) {
        Some("function") if obj.contains_key("name") => {
            let mut function = serde_json::Map::new();
            if let Some(name) = obj.get("name") {
                function.insert("name".to_string(), name.clone());
            }
            if let Some(description) = obj.get("description") {
                function.insert("description".to_string(), description.clone());
            }
            if let Some(parameters) = obj.get("parameters") {
                function.insert("parameters".to_string(), parameters.clone());
            }
            if let Some(strict) = obj.get("strict") {
                function.insert("strict".to_string(), strict.clone());
            }
            vec![json!({"type": "function", "function": Value::Object(function)})]
        }
        // Namespaced tool groups (e.g. multi_agent) bundle inner function tools; flatten them.
        Some("namespace") => obj
            .get("tools")
            .and_then(Value::as_array)
            .map(|inner| {
                inner
                    .iter()
                    .flat_map(chat_tools_from_responses_tool)
                    .collect()
            })
            .unwrap_or_default(),
        // No Chat Completions equivalent (web_search, image_generation, custom, ...): drop.
        _ => Vec::new(),
    }
}

fn push_tool_call_message(messages: &mut Vec<Value>, tool_call: Value, reasoning: Option<&str>) {
    // Chat Completions requires that tool calls are grouped into a single assistant message
    // (with `tool_calls: [...]`) followed by tool role responses.
    if let Some(Value::Object(obj)) = messages.last_mut()
        && obj.get("role").and_then(Value::as_str) == Some("assistant")
        && obj.get("content").is_some_and(Value::is_null)
        && let Some(tool_calls) = obj.get_mut("tool_calls").and_then(Value::as_array_mut)
    {
        tool_calls.push(tool_call);
        if let Some(reasoning) = reasoning {
            if let Some(Value::String(existing)) = obj.get_mut("reasoning") {
                if !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(reasoning);
            } else {
                obj.insert(
                    "reasoning".to_string(),
                    Value::String(reasoning.to_string()),
                );
            }
        }
        return;
    }

    let mut msg = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [tool_call],
    });
    if let Some(reasoning) = reasoning
        && let Some(obj) = msg.as_object_mut()
    {
        obj.insert("reasoning".to_string(), json!(reasoning));
    }
    messages.push(msg);
}
