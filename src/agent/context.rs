//! System prompt construction for the agent loop.
//!
//! Provides [`build_system_prompt`] which assembles a system-prompt [`Message`]
//! with role definition, workspace context, tool descriptions, and editing rules.
//! Supports a `--system-prompt` override (returned verbatim as the system message).

use std::path::Path;

use crate::llm::types::{Message, ToolDef};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the system prompt message for the agent.
///
/// If `custom_prompt` is `Some`, returns it directly as the system message.
/// Otherwise, assembles a default prompt with:
/// - Role definition (coding agent in a workspace)
/// - Workspace root path
/// - Git branch (or "unknown" if not in a git repo)
/// - All tool names and descriptions (iterated from `tool_defs`)
/// - Hash-anchor editing format rules
/// - Edit operation rules (replace, insert_before, insert_after)
/// - Multi-file batching + bottom-to-top ordering
/// - Resource-limited execution rules (30 s timeout, 32 KB cap, no shell interpolation)
pub fn build_system_prompt(
    custom_prompt: Option<&str>,
    tool_defs: &[ToolDef],
    workspace_root: &Path,
    git_branch: &str,
) -> Message {
    let text = match custom_prompt {
        Some(prompt) => prompt.to_string(),
        None => {
            let mut parts: Vec<String> = Vec::new();

            // Role definition
            parts.push(
                "You are a coding agent operating inside a project workspace. \
                 Your job is to help the user understand, modify, and improve \
                 their codebase."
                    .to_string(),
            );

            // Workspace and git context
            parts.push(format!("Workspace root: {}", workspace_root.display()));
            parts.push(format!("Git branch: {git_branch}"));

            // Tool list
            if !tool_defs.is_empty() {
                parts.push("Available tools:".to_string());
                for tool in tool_defs {
                    parts.push(format!("- **{}**: {}", tool.name, tool.description));
                }
            }

            // Anchor format explanation
            parts.push(
                "Every line of code can be referenced by a deterministic, \
                 human-readable word (an \"anchor\"). Anchors are derived from \
                 a 64-bit FNV-1a hash of line content, mapped into a fixed \
                 dictionary of ~1400 common English words. They remain stable \
                 across insertions and deletions elsewhere in the file — \
                 unlike line numbers, which shift."
                    .to_string(),
            );

            parts.push(
                "In file output, each line is prefixed with its anchor followed \
                 by a pipe character (`|`):"
                    .to_string(),
            );
            parts.push("```".to_string());
            parts.push("anchor|line_content".to_string());
            parts.push("```".to_string());

            parts.push(
                "Identical lines (e.g., multiple `}}` or blank lines) are \
                 disambiguated with occurrence-index suffixes: the first keeps \
                 the plain anchor, subsequent lines append `.1`, `.2`, `.3`, etc."
                    .to_string(),
            );

            // Edit operation rules
            parts.push("Supported edit operations:".to_string());
            parts.push(
                "- **replace**: Replace the content of a block identified by an \
                 anchor with new text."
                    .to_string(),
            );
            parts.push(
                "- **insert_before**: Insert new lines before the line identified \
                 by an anchor."
                    .to_string(),
            );
            parts.push(
                "- **insert_after**: Insert new lines after the line identified \
                 by an anchor."
                    .to_string(),
            );
            parts.push(
                "- **replace_symbol**: Replace a named symbol (function, method, class) \
                 using AST-aware byte-range splicing across one or more files."
                    .to_string(),
            );

            // Multi-file batching
            parts.push(
                "All edit operations accept a `files` array — multiple files can \
                 be edited in a single tool call. Edits are applied bottom-to-top \
                 within each file so that earlier edits do not shift anchors for \
                 later edits."
                    .to_string(),
            );

            // Sandboxed execution rules
            parts.push(
                "Command execution is resource-limited with the following rules: \
                 - 30-second timeout \
                 - 32 KB output cap \
                 - No shell interpolation (arguments are passed as a list) \
                 - Working directory is pinned to the workspace root"
                    .to_string(),
            );

            parts.join("\n\n")
        }
    };

    Message::system(text)
}

/// Detect the current git branch. Returns `"unknown"` if not in a git repo or
/// if git fails.
///
/// ⚠️ This is a synchronous, blocking call. Do not call from within the async
/// agent loop — only at startup before the loop begins.
pub fn detect_git_branch(workspace_root: &Path) -> String {
    let output = match std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace_root)
        .output()
    {
        Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            tracing::warn!(
                "git rev-parse exited with status {} in {}",
                out.status,
                workspace_root.display()
            );
            None
        }
        Err(e) => {
            tracing::warn!("git rev-parse failed in {}: {e}", workspace_root.display());
            None
        }
    };
    output.unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(name: &str, description: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: description.into(),
            input_schema: serde_json::json!({}),
        }
    }

    // -- build_system_prompt tests --

    #[test]
    fn test_custom_prompt_overrides_default() {
        let msg = build_system_prompt(Some("custom"), &[], Path::new("/"), "main");
        assert_eq!(msg.role, crate::llm::types::Role::System);
        assert_eq!(msg.content.len(), 1);
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert_eq!(text, "custom");
    }

    #[test]
    fn test_default_prompt_with_empty_tools_has_no_available_tools_section() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            !text.contains("Available tools:"),
            "empty tool list should not include tools section"
        );
    }

    #[test]
    fn test_default_prompt_includes_workspace_root() {
        let ws = Path::new("/tmp/test-workspace");
        let msg = build_system_prompt(None, &[], ws, "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("/tmp/test-workspace"),
            "prompt should contain workspace path"
        );
    }

    #[test]
    fn test_default_prompt_includes_git_branch() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "feature/foo");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("feature/foo"),
            "prompt should contain branch name"
        );
    }

    #[test]
    fn test_default_prompt_includes_tool_names() {
        let tools = vec![
            make_tool("read_file", "Read a file from disk"),
            make_tool("write_file", "Write content to a file"),
        ];
        let msg = build_system_prompt(None, &tools, Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("read_file"),
            "prompt should contain first tool name"
        );
        assert!(
            text.contains("write_file"),
            "prompt should contain second tool name"
        );
        assert!(
            text.contains("Read a file"),
            "prompt should contain first tool description"
        );
        assert!(
            text.contains("Write content"),
            "prompt should contain second tool description"
        );
    }

    #[test]
    fn test_default_prompt_includes_anchor_format() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("anchor"), "prompt should mention anchors");
        assert!(
            text.contains("stable"),
            "prompt should mention stable referencing"
        );
    }

    #[test]
    fn test_default_prompt_includes_editing_rules() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("replace"),
            "prompt should mention replace operation"
        );
        assert!(
            text.contains("insert_before"),
            "prompt should mention insert_before operation"
        );
        assert!(
            text.contains("insert_after"),
            "prompt should mention insert_after operation"
        );
    }

    #[test]
    fn test_default_prompt_includes_multi_file_batching() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(
            text.contains("bottom-to-top"),
            "prompt should mention bottom-to-top ordering"
        );
    }

    #[test]
    fn test_default_prompt_includes_resource_limited_execution() {
        let msg = build_system_prompt(None, &[], Path::new("/"), "main");
        let text = match &msg.content[0].content {
            crate::llm::types::ContentType::Text { text } => text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("30"), "prompt should mention timeout");
        assert!(text.contains("32"), "prompt should mention output cap");
        assert!(
            text.contains("shell interpolation"),
            "prompt should mention no shell interpolation"
        );
    }

    // -- detect_git_branch tests --

    #[test]
    fn test_detect_git_branch_returns_unknown_for_nonexistent_dir() {
        let branch = detect_git_branch(Path::new("/nonexistent/path"));
        assert_eq!(branch, "unknown");
    }

    #[test]
    fn test_detect_git_branch_in_current_repo() {
        // This test runs inside the carv repo, so we expect a real branch name.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let branch = detect_git_branch(repo_root);
        assert_ne!(
            branch, "unknown",
            "should detect a real branch in the carv repo"
        );
        assert!(!branch.is_empty(), "branch name should not be empty");
    }
}
