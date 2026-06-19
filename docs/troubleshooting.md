# Troubleshooting Guide

Common issues when using carv, how to diagnose them, and how to fix them.

## Table of Contents

1. [Installation Issues](#installation-issues)
2. [API Key Issues](#api-key-issues)
3. [Model Errors](#model-errors)
4. [Tool Errors](#tool-errors)
5. [LSP (Planned)](#lsp-planned)
6. [Debugging and Logging](#debugging-and-logging)
7. [FAQ](#faq)

---

## Installation Issues

### Rust version too old

**Error:**
```
error: package `carv v0.1.0` cannot be built because it requires rustc 1.80 or newer
```

**Fix:** Update your Rust toolchain:

```bash
rustup update stable
```

carv requires Rust **1.80+** (specified in `Cargo.toml` as `rust-version = "1.80"`). The project uses native async fn in traits (RPITIT), which was stabilized in 1.75, plus newer features that require 1.80.

Check your current version:

```bash
rustc --version
```

### `cargo install` fails with missing C compiler

**Error (when build deps are enabled):**
```
error: failed to run custom build command for 'tree-sitter-rust v0.24.x'
...
cc: error: no such file or directory
```

**Fix:** Install a C compiler:

```bash
# Debian/Ubuntu
apt install build-essential

# Fedora/RHEL
dnf install gcc

# macOS
xcode-select --install

# Alpine
apk add build-base
```

Tree-sitter grammar crates compile C source at build time. Tree-sitter grammar crates handle their own C compilation handles cross-platform compilation, but a C compiler must be installed on the system.

### Build fails with "the wasm target is not supported"

Some tree-sitter grammar versions expose a WASM feature. If you see:

```
error[E0432]: unresolved import 'tree_sitter_rust::LANGUAGE'
```

Make sure you're using the crate as a Rust library (not the WASM build target). Grammar crates from crates.io should work out of the box — this error typically indicates a version mismatch. See the [README](../README.md) for tested version pairs.

### Cargo build fails with mysterious compile errors

Run a clean build:

```bash
cargo clean && cargo build
```

If the issue persists, check the tree-sitter grammar version compatibility. As noted in `Cargo.toml`:

```toml
tree-sitter = "0.26"
tree-sitter-rust = "0.24"
tree-sitter-python = "0.25"
tree-sitter-typescript = "0.23"
```

Grammar crate versions are semi-independent of the core `tree-sitter` version. If you bump one, you may need to bump all. Check crate compatibility on crates.io.

### Workspace member not found

If you're running carv from within a Cargo workspace and get:

```
error: no matching package named 'carv' found
```

Run from the carv directory directly, or use `cargo build -p carv`.

---

## API Key Issues

### "ANTHROPIC_API_KEY environment variable not set"

**Full error:**
```
Error: ANTHROPIC_API_KEY environment variable not set
```

This occurs when using a model that auto-detects to Anthropic (any model starting with `claude-` or `anthropic/`), or when using `--provider anthropic`, but the environment variable is unset.

**Fix:**

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

Then re-run carv. carv **never** accepts API keys as CLI arguments — they must be environment variables. This prevents accidental exposure through shell history or `/proc`.

### "OPENAI_API_KEY environment variable not set"

**Full error:**
```
Error: OPENAI_API_KEY environment variable not set
```

Same as above, for OpenAI providers (models starting with `gpt-`, `chatgpt-`, `o1-`, `o3-`, `o4-`).

**Fix:**

```bash
export OPENAI_API_KEY="sk-..."
```

### Wrong provider for model

If you set the wrong environment variable for your model — for example, using a Claude model but only setting `OPENAI_API_KEY` — you'll get:

```
Error: ANTHROPIC_API_KEY environment variable not set
```

**Fix:** Either:
1. Set the matching environment variable, or
2. Switch to a model for that provider, or
3. Use `--provider` to override auto-detection (but you'll still need the correct API key for that provider)

### 401 Unauthorized

```
Error: SSE transport error after 0 retries: HTTP status 401
```

Your API key is invalid, revoked, or missing the correct permissions.

**Fix:**
1. Verify the key is set: `echo ${ANTHROPIC_API_KEY:0:8}...`
2. Check the key is active in your Anthropic/OpenAI dashboard
3. Ensure the key has access to the requested model (some keys are restricted to specific models)
4. Regenerate the key if it may have been revoked

---

## Model Errors

### Unknown model — "Cannot auto-detect provider"

**Error:**
```
Error: Cannot auto-detect provider for model 'foo' — use --provider
```

carv auto-detects the provider from well-known model prefixes:
- `claude-*`, `anthropic/*` → Anthropic
- `gpt-*`, `chatgpt-*`, `o1-*`, `o3-*`, `o4-*` → OpenAI
- Everything else → error

**Fix:** Either:
1. Use a recognized model name, or
2. Specify the provider explicitly: `--provider anthropic` or `--provider openai`

Example:
```bash
carv -m my-custom-model --provider anthropic "write a test"
```

### No model specified

**Error:**
```
Error: No model specified. Use -m <MODEL> or --provider <PROVIDER>.
```

You must specify either a model (which auto-detects the provider) or an explicit provider.

**Fix:**

```bash
carv -m claude-sonnet-4-20250514 "implement feature X"
```

or, with just a provider (no model-specific logic is used yet, but both are accepted):

```bash
carv --provider anthropic "hello"
```

### Rate limiting (429 Too Many Requests)

**Log output:**
```
WARN retry_count=0 backoff_ms=1000 error=... "Retryable SSE transport error, backing off"
```

Anthropic and OpenAI return HTTP 429 when you exceed their rate limits. carv retries automatically with exponential backoff (1s → 2s → 4s) for up to 3 attempts.

**If retries are exhausted:**
```
Error: SSE transport error after 3 retries: HTTP status 429
```

**Fix:**
1. Wait a few minutes and try again
2. Reduce the number of concurrent carv sessions
3. Check your API usage dashboard (Anthropic: console.anthropic.com, OpenAI: platform.openai.com)
4. Consider upgrading your API tier if you regularly hit rate limits

### Provider overloaded (529)

**Log output:**
```
WARN retry_count=0 backoff_ms=1000 error=... "Retryable SSE transport error, backing off"
```

HTTP 529 means the provider is temporarily overloaded. carv retries with the same exponential backoff as 429 errors.

### Content filter triggered (OpenAI)

**Error:**
```
Error: content filter triggered
```

OpenAI's content moderation system blocked the response. This can happen with certain prompts.

**Fix:**
1. Rephrase your prompt to avoid triggering content filters
2. If you believe the filter has a false positive, check your OpenAI usage dashboard
3. Consider switching to the `gpt-4o` model, which has less restrictive filtering

### Response truncated by max_tokens

**Error:**
```
Error: response truncated by max_tokens
```

The model hit the `max_tokens` limit before completing its response. This is common for complex tasks that require long output.

**Fix:**
1. carv's agent loop handles this automatically (the LLM continues in the next turn), but if you see frequent truncation, use a model with a larger context window
2. For tool calls truncated by max_tokens, carv handles the truncation within its loop

---

## Tool Errors

### "Missing required 'files' parameter" (edit_file, replace_symbol)

```
edit_file: missing required 'files' parameter
```

The `edit_file` and `replace_symbol` tools require a `files` array. Pass at least one file entry.

**Fix:**
```json
{
  "files": [
    {
      "path": "src/main.rs",
      "edits": [...]
    }
  ]
}
```

### Anchor not found in file

```
edit_file: anchor 'Ancient' not found in 'src/main.rs'. Use read_file to get current anchors.
```

The anchor word you referenced doesn't match any line in the current file. This happens when:
- You used an anchor from a different file
- The file was modified since you read it (anchors may have changed)
- You typed the anchor incorrectly

**Fix:** Run `read_file` on the file again to get fresh anchors.

### "end_anchor comes before anchor"

```
edit_file: end_anchor 'Brave' (line 3) comes before anchor 'Delta' (line 10) in 'src/main.rs'
```

The `end_anchor` must be the same line as `anchor` or a line after it. In `replace` operations, the range goes from `anchor` to `end_anchor`, so `end_anchor` must come later in the file.

### Path escapes workspace root

```
edit_file: path escapes workspace root
```

You specified a path outside the project workspace. carv prevents path traversal attacks.

**Fix:** Use a path within the workspace root. For files outside the workspace, copy them in first.

### Command tool errors

#### "missing required 'command' parameter"

```
Error: missing required 'command' parameter
```

The `execute_command` tool requires a `command` field. Arguments go in a separate `args` array.

**Fix:**
```json
{
  "command": "cargo",
  "args": ["test"]
}
```

#### "command must not be empty"

The command string was empty. Provide a valid command name.

#### "failed to spawn command"

```
Error: failed to spawn command: No such file or directory (os error 2)
```

The command binary was not found. Environment is cleared except for `PATH` and `HOME`. If the command requires other environment variables or is in a non-standard location, use an absolute path or ensure it's on `PATH`.

#### "command timed out after 30s and was killed"

The command exceeded the 30-second timeout limit.

**Fix:** Break the command into smaller steps. For long-running builds, run them outside carv.

#### Output truncated

```
--- (TRUNCATED at 32 KB) ---
```

Command output exceeded 32 KB and was truncated. carv adds a truncation notice at the end. The LLM can see the truncation marker and may request partial output via other tools.

#### No shell interpretation

The `execute_command` tool passes arguments directly via `Command::new(cmd).args(args)`. Shell metacharacters (`|`, `>`, `$`, `;`, etc.) are treated as literal arguments, not operators.

**Wrong:**
```json
{"command": "cargo test | grep passed"}
```

**Correct:**
```json
{"command": "cargo", "args": ["test"]}
```

Pipe the result in your shell, not inside carv.

### Search tool errors

#### "invalid regex pattern"

```
Error: invalid regex pattern: regex parse error: ...
```

The pattern is not a valid regular expression. Fix the regex syntax. carv uses the `grep-regex` crate which follows the Rust regex syntax.

#### "missing required 'pattern' parameter"

The `search_files` tool requires a `pattern` field. Provide a regex string.

### Tree-sitter tool errors

#### "unsupported file extension"

```
get_skeleton failed: unsupported file extension for '/path/to/file.xyz'
```

The file extension is not recognized. Supported extensions: `.rs`, `.py`, `.ts`, `.js`, `.tsx`. For unsupported file types, use `read_file` instead.

#### "symbol 'foo' not found"

```
get_function failed: symbol 'foo' not found in 'src/main.rs'
```

The named function/method was not found in the file. Check:
1. The symbol name is correct
2. The language supports that symbol type
3. The file is parseable by tree-sitter (no syntax errors)

#### "dot-path must have exactly 2 parts"

```
replace_symbol failed: dot-path must have exactly 2 parts, got 'A.B.C'
```

Dot-paths like `A.B.C` with 3+ parts are not supported. Use `Struct.method` (2 parts) for method references.

#### "symbol has no body"

```
get_function failed: symbol 'foo' has no body (may be a forward declaration or trait definition)
```

The found symbol is a declaration only (no body). Examples: trait method signatures in Rust, forward declarations, abstract methods.

#### "Failed to parse tool input JSON"

```
Error: Failed to parse tool input JSON: ...
```

The LLM produced an invalid JSON input for a tool. This typically indicates a provider error — carv continues with the error returned to the LLM, which can retry.

### Path traversal errors

```
write_file failed: path escapes workspace root
list_files failed: path escapes workspace root
replace_symbol failed: path escapes workspace root
```

All file-modifying tools validate that paths stay within the workspace root. Relative paths like `../../etc/passwd` are rejected.

---

## LSP (Planned)

> LSP integration is planned but not yet implemented. The sections below describe expected behavior based on the design doc.

### Language server not found

LSP servers are discovered by path lookup. If a server binary is not on `PATH`, you'll see an error.

**Fix:** Install the language server:

| Language | Server | Installation |
|---|---|---|
| Rust | rust-analyzer | `rustup component add rust-analyzer` |
| Python | `ty` | `pip install ty` |
| TypeScript | `typescript-language-server` | `npm install -g typescript-language-server` |

### Server crash recovery

If an LSP server crashes, carv attempts one restart. If the restart fails, LSP tools for that language are marked unavailable for the session. Tree-sitter operations continue to work.

### Outdated diagnostics

After editing a file, diagnostics may be stale until the server processes the update. carv sends `textDocument/didChange` and waits for the server to acknowledge before returning fresh results.

---

## Debugging and Logging

### Enable verbose output

Use the `-v` / `--verbose` flag:

```bash
carv -v -m claude-sonnet-4-20250514 "write a test"
```

Verbose output goes to **stderr** (not stdout), so it doesn't interfere with programmatic output formats. It includes:
- Tool registration
- Tool execution events
- Tool execution results
- Read-only vs. write tool distinction
- SSE transport errors and retry attempts
- Skipped unparseable SSE events

### Tracing log format

carv uses the `tracing` crate with the `tracing-subscriber` crate for structured logging. Verbose output includes log lines like:

```
WARN retry_count=0 backoff_ms=1000 error=InvalidStatusCode(429) "Retryable SSE transport error, backing off"
DEBUG skips unreadable entry: permission denied
WARN skipping unsearchable file src/binary.bin: unknown file type
```

### What different log levels mean

| Level | Meaning | Example |
|---|---|---|
| `ERROR` | Unrecoverable error, agent may stop | API connection failure |
| `WARN` | Recoverable problem, retried or skipped | Rate limiting, unparseable events |
| `INFO` | Normal operational events | Tool registration, request start |
| `DEBUG` | Detailed internal state | Skipping files, command output |

### Environment variables

| Variable | Required for | Notes |
|---|---|---|
| `ANTHROPIC_API_KEY` | Anthropic provider | Set before running carv |
| `OPENAI_API_KEY` | OpenAI provider | Set before running carv |
| `PATH` | Command execution | Passed through to child processes |
| `HOME` | Command execution | Passed through to child processes |

All other environment variables are **cleared** for child process execution via `env_clear()` to prevent secret leakage. Only `PATH` and `HOME` are explicitly forwarded.

---

## FAQ

### Q: Does carv read stdin?

Yes. If no prompt argument is provided and stdin is piped (not a terminal), carv reads the prompt from stdin:

```bash
echo "review this diff" | carv -m claude-sonnet-4-20250514
git diff | carv -m claude-sonnet-4-20250514 "review changes"
```

### Q: How do I disable dangerous tools?

Use `--disallowed-tools` with a comma-separated list:

```bash
carv -m claude-sonnet-4-20250514 --disallowed-tools "execute_command,edit_file,write_file" "review the code"
```

This is the only way to restrict tool access — tools are auto-approved by default. The `is_read_only()` distinction is informational only.

### Q: How do I get JSON output?

```bash
carv -m claude-sonnet-4-20250514 --output-format json "list files"
```

Available formats:
- `text` (default) — plain text to stdout
- `json` — single JSON object after completion
- `stream-json` — JSON lines as events arrive

### Q: How do I reduce the number of tool-use rounds?

The default is 50 rounds. Use `--max-turns` to limit:

```bash
carv -m claude-sonnet-4-20250514 --max-turns 10 "implement feature"
```

### Q: How do I use a custom system prompt?

```bash
carv -m claude-sonnet-4-20250514 --system-prompt "You are an expert Rust developer." "fix the bug"
```

### Q: How do I use a model with extended thinking (Claude) or reasoning (OpenAI)?

Request configuration supports `thinking` and `thinking_budget` fields. When the provider supports it (Claude with extended thinking, or o-series models with OpenAI), these are sent automatically based on the model configuration.

### Q: Why does my file still show old content after an edit?

The anchor cache is invalidated on file modification, so the next `read_file` call re-reads from disk. If you're seeing stale data, the file was modified externally after the edit. Re-run `read_file` to get fresh content.

### Q: Why does `search_files` miss files that should match?

The `search_files` tool respects `.gitignore` and `.ignore` files. If your search pattern isn't matching, check:

1. The file is not in `.gitignore` or `.ignore`
2. The pattern is a valid regex (carv uses the Rust regex engine)
3. The file is not binary (binary files are skipped)
4. The path scope is correct (`path` parameter defaults to workspace root)

### Q: Can I run carv without a model?

Not yet. carv requires a model (or provider) to function. The CLI enforces this at startup.

### Q: Where does carv store caches?

Caches are in-memory only and live for the duration of the session:
- Anchor cache: per-file line → anchor mappings
- Parser cache: parsed tree-sitter ASTs
- LSP connection state

No data is persisted between sessions.

### Q: Does carv send my code to third parties?

Yes — carv sends code to the LLM provider you choose (Anthropic or OpenAI) as part of the prompt. Treat code sent through carv as visible to the provider. Use `--disallowed-tools` to restrict file-access tools if needed.

### Q: How do I troubleshoot a specific issue?

1. Re-run with `-v` for verbose tracing output
2. Check the exact error message displayed
3. Reduce the problem to a minimal reproduction
4. Run `cargo test` to verify your install is working
5. Open a GitHub issue with the error message and reproduction steps
