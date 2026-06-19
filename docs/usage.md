# User Guide

## Basic Usage

The simplest invocation passes a task prompt directly:

```bash
carv "explain this file"
```

carv parses your prompt, determines what tools are needed, and produces a
response. If no prompt is given on the command line and stdin is piped, carv
reads the prompt from stdin (see [Piping](#piping) below).

## Model Selection

Specify a model with `-m` / `--model`. carv auto-detects the provider from
the model name:

```bash
carv -m claude-sonnet-4-20250514 "refactor this function"
carv -m gpt-4o "review this code"
carv -m o3-mini "analyze the performance"
```

### Auto-detection Rules

| Model prefix          | Provider  |
|-----------------------|-----------|
| `claude-*`            | Anthropic |
| `anthropic/*`         | Anthropic |
| `gpt-*`               | OpenAI    |
| `chatgpt-*`           | OpenAI    |
| `o1-*`, `o3-*`, `o4-*` | OpenAI   |

If the model name does not match any known prefix, use `--provider` to
disambiguate:

```bash
carv -m my-custom-model --provider anthropic "task"
```

### Override Provider

Even with an auto-detectable model, you can force a provider:

```bash
carv -m gpt-4o --provider anthropic "task"
```

This sends the request using the Anthropic API (you must have
`ANTHROPIC_API_KEY` set). Useful for testing or routing.

## Piping

carv reads from stdin when no prompt argument is given and stdin is a pipe.
This is the primary way to feed context:

```bash
# Review a git diff
git diff | carv -m claude-sonnet-4-20250514 "review these changes"

# Explain a file
cat src/main.rs | carv "explain this code"

# Pipe from any command
rg "TODO" src/ | carv "summarize the TODOs"
```

Prompts from stdin are prepended to the user message. You can also combine a
CLI prompt with piped context — the CLI argument becomes the instruction and
the piped content becomes the context:

```bash
# Here, "review these changes" is the instruction and the diff is the context
git diff | carv -m claude-sonnet-4-20250514 "review these changes"
```

## Output Formats

Three output formats are available. The default is `text`.

### text (default)

Prints plain text content as the model produces it. Tool calls are not shown
unless `--verbose` is enabled. Best for interactive terminal use.

```bash
carv -p -m claude-sonnet-4-20250514 "list files in src/"
```

Output:

```
Here are the files in src/:

- src/main.rs
- src/cli.rs
- src/tools/mod.rs
...
```

### json

A single JSON object emitted after the conversation completes. Contains the
full conversation history and token usage. Best for programmatic consumption.

```bash
carv -p -m claude-sonnet-4-20250514 --output-format json "hello"
```

```json
{
  "events": [
    {"type": "text", "content": "Hello!"},
    {"type": "done", "turns": 1, "usage": {"input_tokens": 1200, "output_tokens": 45, "cache_read_tokens": 890}}
  ]
}
```

### stream-json

Newline-delimited JSON events as they happen. Each line is a separate event:

```bash
carv -p -m claude-sonnet-4-20250514 --output-format stream-json "refactor this"
```

```jsonl
{"type":"text","content":"I'll start by reading the file."}
{"type":"thinking","content":"Need to understand the current structure..."}
{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"src/main.rs"}}
{"type":"tool_result","id":"t1","content":"<anchor-prefixed content>"}
{"type":"text","content":"I see the issue. Let me fix it."}
{"type":"tool_use","id":"t2","name":"edit_file","input":{"files":[...]}}
{"type":"tool_result","id":"t2","content":"Applied edits to src/main.rs."}
{"type":"done","turns":2,"usage":{"input_tokens":2400,"output_tokens":560,"cache_read_tokens":890}}
```

Useful for progress bars, logs, or integrating with other tools.

## Non-Interactive Mode

The `-p` / `--print` flag runs carv in non-interactive mode: it prints the
final result to stdout and exits. Without `-p`, carv may enter an interactive
mode (future feature).

```bash
carv -p -m claude-sonnet-4-20250514 "count lines in src/"
```

## Verbose and Debug Output

Enable detailed logging to stderr with `-v` / `--verbose`:

```bash
carv -v -m claude-sonnet-4-20250514 "explain this"
```

This shows:
- Tool call names in the output (e.g. `[tool: read_file]`)
- Debug-level tracing to stderr with agent internals (tool dispatch, token budget decisions, compaction)
- A final summary with total turns and token usage

Note: Extended thinking content is not shown in the output, and token
usage is only reported as a final summary — not per turn. Cache hit/miss
data is not currently emitted.

Useful for understanding what the agent is doing, troubleshooting, or learning
which tools are available.

## Tool Restriction

By default, carv has full access to all tools. Restrict specific tools with
`--disallowed-tools` (comma-separated list of tool names):

```bash
# Disallow write/edit commands — read-only review only
carv -m claude-sonnet-4-20250514 --disallowed-tools "edit_file,execute_command,replace_symbol" "review this project"
```

```bash
# Disallow only command execution
carv -m claude-sonnet-4-20250514 --disallowed-tools "execute_command" "refactor this"
```

### Available Tool Names

| Tool               | Read-only | Purpose                                         |
|--------------------|-----------|-------------------------------------------------|
| `read_file`        | Yes       | Read file with hash-anchored lines              |
| `edit_file`        | No        | Hash-anchored edits (replace/insert)            |
| `search_files`     | Yes       | Content search via ripgrep                      |
| `execute_command`  | No        | Run shell command (resource-limited)            |
| `get_skeleton`     | Yes       | AST structural outline                          |
| `get_function`     | Yes       | Extract function body by name                   |
| `replace_symbol`   | No        | Replace function/class by AST node              |
| `lsp_rename`       | No        | Semantic rename across project (planned)        |
| `lsp_references`   | Yes       | Find all references (planned)                   |
| `lsp_definition`   | Yes       | Go to definition (planned)                      |
| `lsp_diagnostics`  | Yes       | Current type errors/warnings (planned)          |

LSP tools (`lsp_*`) are **planned** — they will be available in a future
release. The tool names are reserved and will work once the feature ships.

## Custom System Prompts

Replace the default system prompt with your own using `--system-prompt`:

```bash
carv -m claude-sonnet-4-20250514 --system-prompt "You are a senior Rust expert. Be concise. Only suggest changes if they are type-safe." "review this module"
```

This is useful for:
- Setting the agent's persona (expert, beginner-friendly, etc.)
- Enforcing conventions (type safety, test coverage, etc.)
- Adding project-specific context
- Reducing verbosity

## Max Turns

Control the maximum number of tool-use rounds (model → tool → model cycles):

```bash
carv -m claude-sonnet-4-20250514 --max-turns 5 "refactor this project"
```

Default is 50. Lower values are useful for quick, focused tasks. Higher values
are needed for complex multi-step operations (refactoring across many files,
debugging with multiple test runs, etc.).

## Common Workflows

### Code Review

```bash
# Review staged changes
git diff --cached | carv -m claude-sonnet-4-20250514 "review the staged changes"

# Review a specific file
carv -p -m claude-sonnet-4-20250514 "review src/main.rs for potential issues"

# Read-only review (no modifications allowed)
carv -m claude-sonnet-4-20250514 \
  --disallowed-tools "edit_file,execute_command,replace_symbol,lsp_rename" \
  "review the src/ directory"
```

### Refactoring

```bash
# Rename a function
carv -m claude-sonnet-4-20250514 "rename the function 'get_data' to 'fetch_data' and update all callers"

# Extract a method
carv -m claude-sonnet-4-20250514 "extract the database connection logic in src/db.rs into a separate function"

# Restructure a module
carv -m gpt-4o "split src/tools/mod.rs into separate files per tool type"
```

### Exploration

```bash
# Get a structural overview
carv -p -m claude-sonnet-4-20250514 "show me the structure of src/"

# Find relevant code
carv -p -m claude-sonnet-4-20250514 "find where errors are handled in the agent loop"

# Trace a function
carv -p -m claude-sonnet-4-20250514 "trace how 'execute_command' works from CLI to execution"

# Understand project architecture
carv -p -m claude-sonnet-4-20250514 "explain the module architecture based on the contents of src/"
```

### Debugging

```bash
# Investigate test failures
CARGO_TARGET_DIR=/tmp/carv-test carv -m o4-mini \
  "run the tests, look at the failures, and fix them" \
  --max-turns 20

# Find and fix lints
cargo clippy 2>&1 | carv -m claude-sonnet-4-20250514 "fix these clippy warnings"

# Debug a runtime issue
carv -m claude-sonnet-4-20250514 "add debug logging to the LSP shutdown path in src/lsp/client.rs"
```

## Tips

### Prompt Engineering

- **Be specific:** "add error handling to the `parse_config` function" is better
  than "improve this code". carv uses tools to read files and make targeted
  changes — a precise prompt reduces unnecessary tool calls.
- **Provide context:** Instead of "fix the bug", describe what you observe:
  "when I pass an empty string to `validate_name`, it panics instead of
  returning an error".
- **Chain tasks:** carv handles multi-step operations. You can ask "read
  `src/main.rs`, identify the config struct, add a new field `timeout: u64`,
  update the parser, and add a test" — it will read, edit, and verify.
- **Multiple files:** carv reads and edits across files. A prompt like "add
  module `src/foo` with a public struct and register it in `lib.rs`" works
  because the agent reads `lib.rs`, understands its structure, and edits it.

### Which Tools to Allow or Disallow

| Task                    | Recommended disallowed tools                     |
|-------------------------|---------------------------------------------------|
| Exploration / reading   | `edit_file,execute_command,replace_symbol`           |
| Code review             | `edit_file,execute_command,replace_symbol`           |
| Refactoring             | None (or `execute_command` if no build needed)    |
| Debugging               | None                                              |
| Learning / onboarding   | `edit_file,execute_command,replace_symbol,lsp_rename`           |
| Automated fixes         | `execute_command` (to prevent running modified code) |

### Output Format per Use Case

- **Interactive terminal:** `text` (default) — readable, streaming
- **Scripting / CI:** `json` — parse the final object for structured data
- **Logging / progress tracking:** `stream-json` — line-delimited events
- **Quick answers:** `text` with `-p` — one-shot, no interactive prompts

### Environment

carv respects standard proxy environment variables (`HTTP_PROXY`,
`HTTPS_PROXY`, `NO_PROXY`, `ALL_PROXY`) for API calls if they are set.
