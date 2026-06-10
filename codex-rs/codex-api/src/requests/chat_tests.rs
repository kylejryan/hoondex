use super::build_chat_completions_body;
use crate::common::ResponsesApiRequest;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn chat_body_keeps_system_prompt_first_and_appends_dynamic_context() {
    let body = build_chat_completions_body(&request(
        "FULL HOONIFY SYSTEM PROMPT",
        vec![
            message("developer", "dynamic repo context"),
            message("user", "current task"),
        ],
    ));

    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "FULL HOONIFY SYSTEM PROMPT"},
            {"role": "system", "content": "dynamic repo context"},
            {"role": "user", "content": "current task"},
        ])
    );
}

#[test]
fn chat_body_adds_tool_use_instructions_when_tools_are_present() {
    let mut request = request(
        "FULL HOONIFY SYSTEM PROMPT",
        vec![
            message("developer", "dynamic repo context"),
            message("user", "current task"),
        ],
    );
    request.tools = vec![json!({
        "type": "function",
        "name": "exec_command",
        "description": "Run a command",
        "parameters": {
            "type": "object",
            "properties": {
                "cmd": {"type": "string"}
            },
            "required": ["cmd"]
        }
    })];

    let body = build_chat_completions_body(&request);

    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "FULL HOONIFY SYSTEM PROMPT"},
            {
                "role": "system",
                "content": super::CHAT_COMPLETIONS_TOOL_INSTRUCTIONS
            },
            {"role": "system", "content": "dynamic repo context"},
            {"role": "user", "content": "current task"},
        ])
    );
    assert_eq!(
        body["tools"],
        json!([{
            "type": "function",
            "function": {
                "name": "exec_command",
                "description": "Run a command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cmd": {"type": "string"}
                    },
                    "required": ["cmd"]
                }
            }
        }])
    );
    assert_eq!(body["tool_choice"], "auto");
}

fn request(instructions: &str, input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "deepseek-ai/DeepSeek-V4-Pro".to_string(),
        instructions: instructions.to_string(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}
