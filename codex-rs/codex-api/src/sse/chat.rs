use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::sse::tool_call_text::extract_text_tool_calls;
use crate::sse::tool_call_text::first_tool_call_marker;
use crate::telemetry::SseTelemetry;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use eventsource_stream::Eventsource;
use futures::Stream;
use futures::StreamExt;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

const REQUEST_ID_HEADER: &str = "x-request-id";

pub(crate) fn spawn_chat_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        process_chat_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

/// Processes Server-Sent Events from the Chat Completions streaming API.
///
/// The upstream protocol terminates a streaming response with a final sentinel event
/// (`data: [DONE]`). Some test stubs emit `data: DONE` (without brackets) instead.
///
/// `eventsource_stream` delivers these sentinels as regular events rather than signaling
/// end-of-stream, so we detect them explicitly and emit `ResponseEvent::Completed`.
pub async fn process_chat_sse<S>(
    stream: S,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<std::sync::Arc<dyn SseTelemetry>>,
) where
    S: Stream<Item = Result<bytes::Bytes, codex_client::TransportError>> + Unpin,
{
    let mut stream = stream.eventsource();

    #[derive(Default, Debug)]
    struct ToolCallState {
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    }

    let mut tool_calls: HashMap<usize, ToolCallState> = HashMap::new();
    let mut tool_call_order: Vec<usize> = Vec::new();
    let mut tool_call_order_seen: HashSet<usize> = HashSet::new();
    let mut tool_call_index_by_id: HashMap<String, usize> = HashMap::new();
    let mut next_tool_call_index = 0usize;
    let mut last_tool_call_index: Option<usize> = None;
    let mut assistant_item: Option<ResponseItem> = None;
    let mut reasoning_item: Option<ResponseItem> = None;
    let mut completed_sent = false;
    // Set once the provider returns a properly structured tool call. While it
    // stays false we treat tool-call markup leaked into assistant text as a
    // fallback signal worth recovering.
    let mut emitted_structured_tool_call = false;

    async fn flush_and_complete(
        tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
        reasoning_item: &mut Option<ResponseItem>,
        assistant_item: &mut Option<ResponseItem>,
        recover_text_tool_calls: bool,
    ) {
        if let Some(reasoning) = reasoning_item.take() {
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(reasoning)))
                .await;
        }

        if let Some(assistant) = assistant_item.take() {
            // The model may have leaked tool calls into the assistant text
            // instead of the structured `tool_calls` field. Recover them so the
            // turn can proceed, keeping any prose written before the leak.
            if recover_text_tool_calls
                && emit_recovered_tool_calls(tx_event, &assistant).await
            {
                send_completed(tx_event).await;
                return;
            }
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(assistant)))
                .await;
        }

        send_completed(tx_event).await;
    }

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                if !completed_sent {
                    flush_and_complete(
                        &tx_event,
                        &mut reasoning_item,
                        &mut assistant_item,
                        !emitted_structured_tool_call,
                    )
                    .await;
                }
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("SSE event: {}", sse.data);

        let data = sse.data.trim();

        if data.is_empty() {
            continue;
        }

        if data == "[DONE]" || data == "DONE" {
            if !completed_sent {
                flush_and_complete(
                    &tx_event,
                    &mut reasoning_item,
                    &mut assistant_item,
                    !emitted_structured_tool_call,
                )
                .await;
            }
            return;
        }

        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(val) => val,
            Err(err) => {
                debug!("Failed to parse ChatCompletions SSE event: {err}, data: {data}");
                continue;
            }
        };

        let Some(choices) = value.get("choices").and_then(|c| c.as_array()) else {
            continue;
        };

        for choice in choices {
            if let Some(delta) = choice.get("delta") {
                if let Some(reasoning) = delta.get("reasoning") {
                    if let Some(text) = reasoning.as_str() {
                        append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string())
                            .await;
                    } else if let Some(text) = reasoning.get("text").and_then(|v| v.as_str()) {
                        append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string())
                            .await;
                    } else if let Some(text) = reasoning.get("content").and_then(|v| v.as_str()) {
                        append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string())
                            .await;
                    }
                }

                if let Some(content) = delta.get("content") {
                    if let Some(arr) = content.as_array() {
                        for item in arr {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                append_assistant_text(
                                    &tx_event,
                                    &mut assistant_item,
                                    text.to_string(),
                                )
                                .await;
                            }
                        }
                    } else if let Some(text) = content.as_str() {
                        append_assistant_text(&tx_event, &mut assistant_item, text.to_string())
                            .await;
                    }
                }

                if let Some(tool_call_values) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                    for tool_call in tool_call_values {
                        let mut index = tool_call
                            .get("index")
                            .and_then(serde_json::Value::as_u64)
                            .map(|i| i as usize);

                        let mut call_id_for_lookup = None;
                        if let Some(call_id) = tool_call.get("id").and_then(|i| i.as_str()) {
                            call_id_for_lookup = Some(call_id.to_string());
                            if let Some(existing) = tool_call_index_by_id.get(call_id) {
                                index = Some(*existing);
                            }
                        }

                        if index.is_none() && call_id_for_lookup.is_none() {
                            index = last_tool_call_index;
                        }

                        let index = index.unwrap_or_else(|| {
                            while tool_calls.contains_key(&next_tool_call_index) {
                                next_tool_call_index += 1;
                            }
                            let idx = next_tool_call_index;
                            next_tool_call_index += 1;
                            idx
                        });

                        let call_state = tool_calls.entry(index).or_default();
                        if tool_call_order_seen.insert(index) {
                            tool_call_order.push(index);
                        }

                        if let Some(id) = tool_call.get("id").and_then(|i| i.as_str()) {
                            call_state.id.get_or_insert_with(|| id.to_string());
                            tool_call_index_by_id.entry(id.to_string()).or_insert(index);
                        }

                        if let Some(func) = tool_call.get("function") {
                            if let Some(fname) = func.get("name").and_then(|n| n.as_str())
                                && !fname.is_empty()
                            {
                                call_state.name.get_or_insert_with(|| fname.to_string());
                            }
                            if let Some(arguments) = func.get("arguments").and_then(|a| a.as_str()) {
                                call_state.arguments.push_str(arguments);
                            }
                        }

                        last_tool_call_index = Some(index);
                    }
                }
            }

            if let Some(message) = choice.get("message")
                && let Some(reasoning) = message.get("reasoning")
            {
                if let Some(text) = reasoning.as_str() {
                    append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string()).await;
                } else if let Some(text) = reasoning.get("text").and_then(|v| v.as_str()) {
                    append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string()).await;
                } else if let Some(text) = reasoning.get("content").and_then(|v| v.as_str()) {
                    append_reasoning_text(&tx_event, &mut reasoning_item, text.to_string()).await;
                }
            }

            let finish_reason = choice.get("finish_reason").and_then(|r| r.as_str());
            if finish_reason == Some("stop") {
                if let Some(reasoning) = reasoning_item.take() {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(reasoning)))
                        .await;
                }

                if let Some(assistant) = assistant_item.take() {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(assistant)))
                        .await;
                }
                if !completed_sent {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id: String::new(),
                            token_usage: None,
                            end_turn: Some(true),
                        }))
                        .await;
                    completed_sent = true;
                }
                continue;
            }

            if finish_reason == Some("length") {
                let _ = tx_event.send(Err(ApiError::ContextWindowExceeded)).await;
                return;
            }

            if finish_reason == Some("tool_calls") {
                if let Some(reasoning) = reasoning_item.take() {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(reasoning)))
                        .await;
                }

                for index in tool_call_order.drain(..) {
                    let Some(state) = tool_calls.remove(&index) else {
                        continue;
                    };
                    tool_call_order_seen.remove(&index);
                    let ToolCallState {
                        id,
                        name,
                        arguments,
                    } = state;
                    let Some(name) = name else {
                        debug!("Skipping tool call at index {index} because name is missing");
                        continue;
                    };
                    let item = ResponseItem::FunctionCall {
                        id: None,
                        name,
                        namespace: None,
                        arguments,
                        call_id: id.unwrap_or_else(|| format!("tool-call-{index}")),
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                    emitted_structured_tool_call = true;
                }
            }
        }
    }
}

async fn send_completed(tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>) {
    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: String::new(),
            token_usage: None,
            end_turn: None,
        }))
        .await;
}

/// Concatenate the `OutputText` parts of an assistant message.
fn assistant_output_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::OutputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// If `assistant` carries tool-call markup the provider failed to structure,
/// emit recovered `FunctionCall` items (plus any leading prose) and return
/// `true`. The original assistant message is then dropped by the caller.
async fn emit_recovered_tool_calls(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant: &ResponseItem,
) -> bool {
    let ResponseItem::Message { content, .. } = assistant else {
        return false;
    };
    let text = assistant_output_text(content);
    let calls = extract_text_tool_calls(&text);
    if calls.is_empty() {
        return false;
    }

    if let Some(idx) = first_tool_call_marker(&text) {
        let prose = text[..idx].trim();
        if !prose.is_empty() {
            let _ = tx_event
                .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: prose.to_string(),
                    }],
                    phase: None,
                })))
                .await;
        }
    }

    for (index, call) in calls.into_iter().enumerate() {
        let item = ResponseItem::FunctionCall {
            id: None,
            name: call.name,
            namespace: None,
            arguments: call.arguments,
            call_id: format!("text-tool-call-{index}"),
        };
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await;
    }
    true
}

async fn append_assistant_text(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_item: &mut Option<ResponseItem>,
    text: String,
) {
    if assistant_item.is_none() {
        let item = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![],
            phase: None,
        };
        *assistant_item = Some(item.clone());
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemAdded(item)))
            .await;
    }

    if let Some(ResponseItem::Message { content, .. }) = assistant_item {
        content.push(ContentItem::OutputText { text: text.clone() });
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputTextDelta(text)))
            .await;
    }
}

async fn append_reasoning_text(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    reasoning_item: &mut Option<ResponseItem>,
    text: String,
) {
    if reasoning_item.is_none() {
        let item = ResponseItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            content: Some(vec![]),
            encrypted_content: None,
        };
        *reasoning_item = Some(item.clone());
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemAdded(item)))
            .await;
    }

    if let Some(ResponseItem::Reasoning {
        content: Some(content),
        ..
    }) = reasoning_item
    {
        let content_index = content.len() as i64;
        content.push(ReasoningItemContent::ReasoningText { text: text.clone() });

        let _ = tx_event
            .send(Ok(ResponseEvent::ReasoningContentDelta {
                delta: text,
                content_index,
            }))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use codex_protocol::models::ResponseItem;
    use futures::TryStreamExt;
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio_util::io::ReaderStream;

    fn build_body(events: &[serde_json::Value]) -> String {
        let mut body = String::new();
        for e in events {
            body.push_str(&format!("event: message\ndata: {e}\n\n"));
        }
        body
    }

    #[tokio::test]
    async fn completes_on_done_sentinel_without_json() {
        let events = collect_events("event: message\ndata: [DONE]\n\n").await;
        assert_matches!(&events[..], [ResponseEvent::Completed { .. }]);
    }

    async fn collect_events(body: &str) -> Vec<ResponseEvent> {
        let reader = ReaderStream::new(std::io::Cursor::new(body.to_string()))
            .map_err(|err| codex_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(process_chat_sse(
            reader,
            tx,
            Duration::from_millis(1000),
            None,
        ));

        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev.expect("stream error"));
        }
        out
    }

    #[tokio::test]
    async fn concatenates_tool_call_arguments_across_deltas() {
        let delta_name = json!({
            "choices": [{
                "delta": { "tool_calls": [{ "id": "call_a", "index": 0, "function": { "name": "do_a" } }] }
            }]
        });
        let delta_args_1 = json!({
            "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "{ \"foo\":" } }] } }]
        });
        let delta_args_2 = json!({
            "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "1}" } }] } }]
        });
        let finish = json!({ "choices": [{ "finish_reason": "tool_calls" }] });

        let body = build_body(&[delta_name, delta_args_1, delta_args_2, finish]);
        let events = collect_events(&body).await;
        assert_matches!(
            &events[..],
            [
                ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, name, arguments, .. }),
                ResponseEvent::Completed { .. }
            ] if call_id == "call_a" && name == "do_a" && arguments == "{ \"foo\":1}"
        );
    }

    #[tokio::test]
    async fn drops_partial_tool_calls_on_stop_finish_reason() {
        let delta_tool = json!({
            "choices": [{ "delta": { "tool_calls": [{ "id": "call_a", "function": { "name": "do_a", "arguments": "{}" } }] } }]
        });
        let finish_stop = json!({ "choices": [{ "finish_reason": "stop" }] });

        let body = build_body(&[delta_tool, finish_stop]);
        let events = collect_events(&body).await;

        assert!(!events.iter().any(|ev| {
            matches!(
                ev,
                ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
            )
        }));
        assert_matches!(events.last(), Some(ResponseEvent::Completed { .. }));
    }
}
