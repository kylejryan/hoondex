use super::*;
use serde_json::Value;

fn args(call: &ExtractedToolCall) -> Value {
    serde_json::from_str(&call.arguments).unwrap()
}

#[test]
fn parses_anthropic_invoke_with_string_attribute() {
    // The exact shape DeepSeek leaked, including the bogus `string="true"`.
    let text = r#"Let me look.
<invoke name="shell">
<parameter name="cmd" string="true">find /repo -name "*.rs" | head -50</parameter>
<parameter name="workdir" string="true">/repo</parameter>
</invoke>"#;
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    let a = args(&calls[0]);
    assert_eq!(a["cmd"], "find /repo -name \"*.rs\" | head -50");
    assert_eq!(a["workdir"], "/repo");
}

#[test]
fn parses_multiple_consecutive_invokes() {
    let text = r#"<invoke name="shell">
<parameter name="cmd">ls</parameter>
</invoke>
<invoke name="shell">
<parameter name="cmd">pwd</parameter>
</invoke>
</tool_calls>"#;
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(args(&calls[0])["cmd"], "ls");
    assert_eq!(args(&calls[1])["cmd"], "pwd");
}

#[test]
fn tolerates_missing_close_invoke() {
    let text = r#"<invoke name="read_file">
<parameter name="filePath">/etc/hosts</parameter>"#;
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(args(&calls[0])["filePath"], "/etc/hosts");
}

#[test]
fn preserves_inner_angle_brackets_and_newlines() {
    let text = "<invoke name=\"shell\">\n\
        <parameter name=\"cmd\">grep -n \"a < b && c > d\" file\nsecond line</parameter>\n\
        </invoke>";
    let calls = extract_text_tool_calls(text);
    assert_eq!(args(&calls[0])["cmd"], "grep -n \"a < b && c > d\" file\nsecond line");
}

#[test]
fn parses_single_quoted_attributes() {
    let text = "<invoke name='shell'><parameter name='cmd'>echo hi</parameter></invoke>";
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(args(&calls[0])["cmd"], "echo hi");
}

#[test]
fn parses_tool_call_json_block() {
    let text = r#"<tool_call>{"name": "shell", "arguments": {"cmd": "ls -la"}}</tool_call>"#;
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(args(&calls[0])["cmd"], "ls -la");
}

#[test]
fn tool_call_json_with_string_arguments() {
    let text = r#"<tool_call>{"name": "bash", "arguments": "{\"command\":\"ls\"}"}</tool_call>"#;
    let calls = extract_text_tool_calls(text);
    assert_eq!(calls[0].name, "bash");
    assert_eq!(args(&calls[0])["command"], "ls");
}

#[test]
fn plain_prose_yields_nothing() {
    let calls = extract_text_tool_calls("Sure, I'll read the README and summarize it.");
    assert!(calls.is_empty());
    assert!(first_tool_call_marker("just prose").is_none());
}

#[test]
fn first_marker_locates_prose_boundary() {
    let text = "Here is my plan.\n<invoke name=\"shell\"><parameter name=\"cmd\">ls</parameter></invoke>";
    let idx = first_tool_call_marker(text).unwrap();
    assert_eq!(&text[..idx], "Here is my plan.\n");
}

#[test]
fn invoke_without_name_is_skipped() {
    let text = r#"<invoke><parameter name="cmd">ls</parameter></invoke>"#;
    assert!(extract_text_tool_calls(text).is_empty());
}
