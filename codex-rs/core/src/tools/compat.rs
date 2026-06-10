//! Tool-call compatibility shim for non-OpenAI models.
//!
//! Codex exposes a small, fixed toolset (`shell_command`/`exec_command`,
//! `apply_patch`, `update_plan`, ...) and does *all* file work through the
//! shell. Models served via Chat Completions gateways such as Hoonify
//! (DeepSeek V4 Pro) are not tuned on that schema: they emit tool names and
//! argument shapes borrowed from other harnesses — Claude Code's
//! `bash`/`read`/`write`, Cursor's `read_file`/`todo_write`, Gemini's
//! `run_shell_command`, and so on. Those calls hit the registry as misses and
//! used to return a bare `unsupported call: <name>`, leaving the model to spin.
//!
//! [`resolve_tool_compat`] translates that foreign vocabulary onto the tools
//! that are actually registered for the turn. It runs **only on a registry
//! miss**, so a model that already emits canonical names is never touched.
//! File-shaped tools (`read`/`ls`/`grep`/`write`/`edit`) are lowered to shell
//! commands because that is how Codex performs those operations natively.

use std::collections::HashSet;

use codex_tools::ToolName;
use serde_json::Map;
use serde_json::Value;

use crate::tools::context::ToolPayload;

/// Canonical shell tools in preference order, paired with the JSON key each one
/// uses for the command string. Both may be registered at once (unified exec
/// keeps `shell_command` as a dispatch-only fallback), so order decides which
/// the shim prefers.
const SHELL_TARGETS: [(&str, &str); 2] = [("exec_command", "cmd"), ("shell_command", "command")];

/// Attempt to translate a tool call the registry does not recognize into an
/// equivalent call against a registered tool.
///
/// `registered` is the set of flat tool names currently in the registry.
/// Returns the rewritten `(name, payload)` on success, or `None` when the call
/// cannot be mapped (the caller should then surface [`unknown_tool_message`]).
pub(crate) fn resolve_tool_compat(
    registered: &HashSet<String>,
    requested: &ToolName,
    payload: &ToolPayload,
) -> Option<(ToolName, ToolPayload)> {
    // Only JSON function calls carry arguments we know how to rewrite. Custom
    // (freeform) and tool-search payloads are left alone.
    let ToolPayload::Function { arguments } = payload else {
        return None;
    };
    let args = parse_args(arguments);
    let normalized = requested.name.trim().to_ascii_lowercase();

    match classify(&normalized)? {
        Family::Shell => {
            let command = first_str(
                &args,
                &[
                    "command",
                    "cmd",
                    "script",
                    "input",
                    "code",
                    "shell_command",
                    "bash_command",
                    "commandline",
                ],
            )?;
            let workdir = first_str(
                &args,
                &["workdir", "directory", "dir", "cwd", "working_directory"],
            );
            shell_call(registered, command, workdir)
        }
        Family::Read => shell_from_command(registered, read_command(&args)?),
        Family::List => shell_from_command(registered, list_command(&args)),
        Family::Search => shell_from_command(registered, search_command(&args)?),
        Family::Write => shell_from_command(registered, write_or_edit_command(&args)?),
        Family::Plan => plan_call(registered, &args),
    }
}

/// Human-readable error for a tool call that could neither be found nor
/// translated. Unlike the old bare message, this enumerates the real tool
/// names so the model can correct itself on the next turn.
pub(crate) fn unknown_tool_message(requested: &ToolName, registered: &HashSet<String>) -> String {
    let mut names: Vec<&str> = registered.iter().map(String::as_str).collect();
    names.sort_unstable();
    format!(
        "unknown tool `{requested}`. This environment exposes a fixed tool set; \
         call one of these exact tool names: {}. Use the shell tool for file work \
         (read with `cat`, list with `ls`, search with `grep`, write via redirection) \
         and `apply_patch` to edit files.",
        names.join(", ")
    )
}

enum Family {
    Shell,
    Read,
    List,
    Search,
    /// Both file creation and in-place edits; both lower to a shell command.
    Write,
    Plan,
}

fn classify(name: &str) -> Option<Family> {
    let family = match name {
        "bash" | "sh" | "zsh" | "shell" | "exec" | "execute" | "execute_command" | "run"
        | "run_command" | "run_shell" | "run_shell_command" | "run_terminal_cmd" | "terminal"
        | "command" | "console" | "system" | "shell_exec" => Family::Shell,

        "read" | "read_file" | "readfile" | "cat" | "view" | "view_file" | "open" | "open_file"
        | "get_file" | "show_file" | "fs_read" => Family::Read,

        "ls" | "list" | "list_files" | "list_dir" | "list_directory" | "glob" | "dir" | "tree"
        | "fs_list" => Family::List,

        "grep"
        | "search"
        | "ripgrep"
        | "rg"
        | "search_file_contents"
        | "search_files"
        | "find_in_files"
        | "codebase_search"
        | "grep_search" => Family::Search,

        "write"
        | "write_file"
        | "writefile"
        | "create_file"
        | "create"
        | "save_file"
        | "put_file"
        | "fs_write"
        | "edit"
        | "edit_file"
        | "str_replace"
        | "str_replace_editor"
        | "str_replace_based_edit_tool"
        | "apply_diff"
        | "replace"
        | "replace_in_file"
        | "modify_file"
        | "search_replace" => Family::Write,

        "todo_write" | "write_todos" | "update_todo" | "update_todos" | "set_plan"
        | "manage_todo_list" | "todos" => Family::Plan,

        _ => return None,
    };
    Some(family)
}

/// Locate the preferred registered shell tool and its command-argument key.
fn shell_target(registered: &HashSet<String>) -> Option<(ToolName, &'static str)> {
    SHELL_TARGETS
        .iter()
        .find(|(name, _)| registered.contains(*name))
        .map(|(name, key)| (ToolName::plain(*name), *key))
}

fn shell_call(
    registered: &HashSet<String>,
    command: String,
    workdir: Option<String>,
) -> Option<(ToolName, ToolPayload)> {
    let (name, key) = shell_target(registered)?;
    let mut obj = Map::new();
    obj.insert(key.to_string(), Value::String(command));
    if let Some(workdir) = workdir {
        obj.insert("workdir".to_string(), Value::String(workdir));
    }
    Some((name, function_payload(obj)))
}

fn shell_from_command(
    registered: &HashSet<String>,
    command: String,
) -> Option<(ToolName, ToolPayload)> {
    shell_call(registered, command, None)
}

fn read_command(args: &Value) -> Option<String> {
    let path = first_str(
        args,
        &[
            "file_path",
            "filepath",
            "path",
            "file",
            "target_file",
            "filename",
            "abs_path",
        ],
    )?;
    Some(format!("cat -- {}", shell_quote(&path)))
}

fn list_command(args: &Value) -> String {
    let path = first_str(args, &["path", "dir", "directory", "target_directory"])
        .unwrap_or_else(|| ".".to_string());
    format!("ls -la -- {}", shell_quote(&path))
}

fn search_command(args: &Value) -> Option<String> {
    let pattern = first_str(args, &["pattern", "query", "regex", "search", "q"])?;
    let path = first_str(args, &["path", "dir", "directory", "include"])
        .unwrap_or_else(|| ".".to_string());
    Some(format!(
        "grep -rnI -e {} -- {}",
        shell_quote(&pattern),
        shell_quote(&path)
    ))
}

/// Lower a write or edit call to a shell command.
///
/// * If `old`/`new` strings are present, perform an exact single-occurrence
///   replacement via `python3` (strings passed as argv, so no quoting hazards).
/// * Otherwise treat it as a full-content write: create parent dirs and write
///   the file with `printf`.
fn write_or_edit_command(args: &Value) -> Option<String> {
    let path = first_str(
        args,
        &[
            "file_path",
            "filepath",
            "path",
            "file",
            "target_file",
            "filename",
        ],
    )?;

    let old = first_str(args, &["old_string", "old_str", "old", "find", "old_text"]);
    let new = first_str(
        args,
        &["new_string", "new_str", "new", "replacement", "new_text"],
    );

    if let Some(old) = old {
        let new = new.unwrap_or_default();
        // Multi-statement python via argv; newline-safe and quote-safe.
        let script = "import sys\n\
             p, o, n = sys.argv[1], sys.argv[2], sys.argv[3]\n\
             s = open(p).read()\n\
             if o not in s:\n    \
             sys.stderr.write('old_string not found in ' + p + '\\n'); sys.exit(1)\n\
             open(p, 'w').write(s.replace(o, n, 1))\n\
             print('edited ' + p)";
        return Some(format!(
            "python3 -c {} {} {} {}",
            shell_quote(script),
            shell_quote(&path),
            shell_quote(&old),
            shell_quote(&new),
        ));
    }

    let content = first_str(
        args,
        &["content", "contents", "text", "file_text", "data", "body"],
    )
    .unwrap_or_default();
    let mkdir = match parent_dir(&path) {
        Some(parent) => format!("mkdir -p -- {} 2>/dev/null; ", shell_quote(&parent)),
        None => String::new(),
    };
    Some(format!(
        "{mkdir}printf '%s' {} > {}",
        shell_quote(&content),
        shell_quote(&path)
    ))
}

fn plan_call(registered: &HashSet<String>, args: &Value) -> Option<(ToolName, ToolPayload)> {
    if !registered.contains("update_plan") {
        return None;
    }
    let items = ["todos", "plan", "items", "tasks"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_array))?;

    let plan: Vec<Value> = items
        .iter()
        .filter_map(|item| {
            let step = first_str(
                item,
                &[
                    "step",
                    "content",
                    "title",
                    "text",
                    "task",
                    "name",
                    "description",
                ],
            )?;
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(normalize_status)
                .unwrap_or("pending");
            Some(Value::Object(Map::from_iter([
                ("step".to_string(), Value::String(step)),
                ("status".to_string(), Value::String(status.to_string())),
            ])))
        })
        .collect();

    if plan.is_empty() {
        return None;
    }

    let mut obj = Map::new();
    if let Some(explanation) = first_str(args, &["explanation", "summary", "note"]) {
        obj.insert("explanation".to_string(), Value::String(explanation));
    }
    obj.insert("plan".to_string(), Value::Array(plan));
    Some((ToolName::plain("update_plan"), function_payload(obj)))
}

fn normalize_status(status: &str) -> &'static str {
    match status
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "completed" | "complete" | "done" | "finished" => "completed",
        "in_progress" | "inprogress" | "doing" | "active" | "started" | "running" | "current" => {
            "in_progress"
        }
        _ => "pending",
    }
}

fn function_payload(obj: Map<String, Value>) -> ToolPayload {
    ToolPayload::Function {
        arguments: Value::Object(obj).to_string(),
    }
}

fn parse_args(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(arguments).unwrap_or(Value::Null)
}

/// First non-empty string value among `keys`. Keys are matched
/// case-insensitively so `filePath`, `file_path`, and `FilePath` all resolve
/// against the candidate `file_path` — models vary in their casing.
fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    keys.iter().find_map(|key| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Parent directory of a `/`-separated path, or `None` when there is no
/// meaningful parent to create.
fn parent_dir(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    let parent = &trimmed[..idx];
    if parent.is_empty() || parent == "." {
        None
    } else {
        Some(parent.to_string())
    }
}

/// POSIX single-quote escaping: wrap in `'...'`, replacing embedded `'` with
/// the `'\''` idiom. Safe for arbitrary content including newlines.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
#[path = "compat_tests.rs"]
mod compat_tests;
