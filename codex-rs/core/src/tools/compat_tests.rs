use super::*;
use serde_json::json;

fn registered(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn func(args: Value) -> ToolPayload {
    ToolPayload::Function {
        arguments: args.to_string(),
    }
}

fn resolve(reg: &[&str], name: &str, args: Value) -> Option<(ToolName, Value)> {
    let (tool, payload) =
        resolve_tool_compat(&registered(reg), &ToolName::plain(name), &func(args))?;
    let ToolPayload::Function { arguments } = payload else {
        panic!("expected function payload");
    };
    Some((tool, serde_json::from_str(&arguments).unwrap()))
}

#[test]
fn bash_maps_to_shell_command_command_key() {
    let (tool, args) = resolve(&["shell_command"], "bash", json!({"command": "ls -la"})).unwrap();
    assert_eq!(tool, ToolName::plain("shell_command"));
    assert_eq!(args["command"], "ls -la");
}

#[test]
fn run_shell_command_maps_to_exec_command_cmd_key_and_workdir() {
    let (tool, args) = resolve(
        &["exec_command", "shell_command"],
        "run_shell_command",
        json!({"command": "echo hi", "directory": "/tmp"}),
    )
    .unwrap();
    // exec_command is preferred and uses the `cmd` key.
    assert_eq!(tool, ToolName::plain("exec_command"));
    assert_eq!(args["cmd"], "echo hi");
    assert_eq!(args["workdir"], "/tmp");
}

#[test]
fn read_lowers_to_cat_through_shell() {
    let (tool, args) = resolve(
        &["shell_command"],
        "read_file",
        json!({"filePath": "/etc/hosts"}),
    )
    .unwrap();
    assert_eq!(tool, ToolName::plain("shell_command"));
    assert_eq!(args["command"], "cat -- '/etc/hosts'");
}

#[test]
fn ls_defaults_to_cwd() {
    let (_, args) = resolve(&["shell_command"], "list_files", json!({})).unwrap();
    assert_eq!(args["command"], "ls -la -- '.'");
}

#[test]
fn grep_builds_search_command() {
    let (_, args) = resolve(
        &["shell_command"],
        "search_file_contents",
        json!({"pattern": "TODO", "path": "src"}),
    )
    .unwrap();
    assert_eq!(args["command"], "grep -rnI -e 'TODO' -- 'src'");
}

#[test]
fn write_creates_parent_and_writes_content() {
    let (_, args) = resolve(
        &["shell_command"],
        "write_file",
        json!({"path": "a/b/c.txt", "content": "hello"}),
    )
    .unwrap();
    assert_eq!(
        args["command"],
        "mkdir -p -- 'a/b' 2>/dev/null; printf '%s' 'hello' > 'a/b/c.txt'"
    );
}

#[test]
fn edit_uses_python_exact_replace() {
    let (_, args) = resolve(
        &["shell_command"],
        "str_replace",
        json!({"path": "f.py", "old_string": "a", "new_string": "b"}),
    )
    .unwrap();
    let cmd = args["command"].as_str().unwrap();
    assert!(cmd.starts_with("python3 -c "));
    assert!(cmd.ends_with(" 'f.py' 'a' 'b'"));
}

#[test]
fn single_quotes_in_paths_are_escaped() {
    let (_, args) = resolve(&["shell_command"], "read", json!({"path": "a'b.txt"})).unwrap();
    assert_eq!(args["command"], "cat -- 'a'\\''b.txt'");
}

#[test]
fn todo_write_maps_to_update_plan_with_normalized_status() {
    let (tool, args) = resolve(
        &["update_plan", "shell_command"],
        "todo_write",
        json!({"todos": [
            {"content": "do thing", "status": "in-progress"},
            {"content": "done thing", "status": "done"},
            {"title": "later"}
        ]}),
    )
    .unwrap();
    assert_eq!(tool, ToolName::plain("update_plan"));
    let plan = args["plan"].as_array().unwrap();
    assert_eq!(plan[0]["step"], "do thing");
    assert_eq!(plan[0]["status"], "in_progress");
    assert_eq!(plan[1]["status"], "completed");
    assert_eq!(plan[2]["step"], "later");
    assert_eq!(plan[2]["status"], "pending");
}

#[test]
fn unknown_tool_returns_none() {
    assert!(resolve(&["shell_command"], "frobnicate", json!({})).is_none());
}

#[test]
fn plan_without_shell_target_still_maps() {
    // update_plan present but no shell tool: plan mapping is independent.
    assert!(
        resolve(
            &["update_plan"],
            "todo_write",
            json!({"todos": [{"content": "x"}]})
        )
        .is_some()
    );
}

#[test]
fn shell_family_requires_a_registered_shell_tool() {
    // No shell tool registered -> cannot map a bash call.
    assert!(resolve(&["update_plan"], "bash", json!({"command": "ls"})).is_none());
}

#[test]
fn custom_payload_is_not_translated() {
    let payload = ToolPayload::Custom {
        input: "patch".to_string(),
    };
    assert!(
        resolve_tool_compat(
            &registered(&["shell_command"]),
            &ToolName::plain("write"),
            &payload
        )
        .is_none()
    );
}

#[test]
fn unknown_tool_message_lists_registered_names() {
    let msg = unknown_tool_message(
        &ToolName::plain("frob"),
        &registered(&["shell_command", "apply_patch"]),
    );
    assert!(msg.contains("frob"));
    assert!(msg.contains("apply_patch"));
    assert!(msg.contains("shell_command"));
}
