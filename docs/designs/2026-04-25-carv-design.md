# carv — Minimal Rust Coding Agent with Tree-sitter + LSP

## Context

Dirac demonstrated that AST-level tools — hash-anchored edits, tree-sitter structural operations (`replace_symbol`, `get_function`, `get_file_skeleton`), and multi-file batching — dramatically reduce token cost and improve accuracy compared to naive line-based editing. Its approach keeps context tightly curated so the LLM reasons at the right abstraction level.

**carv** builds on this insight with two additions: **LSP semantic understanding** (scope-aware rename, cross-file reference finding, real type diagnostics) layered on top of tree-sitter's structural precision, and a **minimal standalone Rust CLI** rather than a VS Code extension. The result is a headless, streaming coding agent that combines fast syntactic operations with semantic correctness.

## Requirements

- Single Rust binary, monolith architecture
- Non-interactive by default, fully automated (opt-out via `--disallowed-tools`)
- Streaming output compatible with claude-code conventions
- Dual LLM provider support from day 0: Anthropic (with prompt caching) + OpenAI
- Extended thinking / reasoning support for both providers
- Hash-anchored line referencing for stable, precise edits (with duplicate-line disambiguation)
- Multi-file batched edits in a single tool call
- Tree-sitter support for Rust, Python, TypeScript
- LSP support for rust-analyzer, ty, typescript-language-server
- Auto-detect project languages and lazily spawn appropriate servers
- Token budget tracking and context window management
- Piped stdin support (`git diff | carv "review"`)

## Architecture

```
src/
├── main.rs               # CLI parsing (clap), entrypoint
├── cli.rs                # Arg definitions, config loading
├── llm/
│   ├── mod.rs
│   ├── provider.rs       # LlmProvider trait (native async, no async-trait)
│   ├── types.rs          # Message, ToolDef, StreamEvent, RequestConfig, Usage
│   ├── anthropic.rs      # Anthropic /v1/messages SSE client (with prompt caching)
│   └── openai.rs         # OpenAI /v1/chat/completions SSE client
├── tools/
│   ├── mod.rs
│   ├── registry.rs       # Tool registry, dispatch, deny-list filtering
│   ├── traits.rs         # Tool trait definition
│   ├── fs.rs             # read_file, write_file, list_files (.gitignore-aware)
│   ├── edit.rs           # edit_file (hash-anchored, multi-file batched edits)
│   ├── exec.rs           # execute_command (sandboxed: timeout, cwd, output cap)
│   ├── treesitter.rs     # get_skeleton, get_function, replace_symbol
│   ├── lsp.rs            # lsp_rename, lsp_references, lsp_definition, lsp_diagnostics
│   └── search.rs         # search_files (.gitignore-aware, ripgrep-based)
├── lsp/
│   ├── mod.rs
│   ├── client.rs         # Manage a single language server (tokio::process)
│   ├── registry.rs       # Language detection, server config mapping
│   └── transport.rs      # JSON-RPC over stdio (split stdin/stdout handles)
├── hashing/
│   ├── mod.rs
│   ├── anchors.rs        # Anchor generation (word-based hashes + occurrence index)
│   └── state.rs          # Per-file anchor state manager
├── treesitter/
│   ├── mod.rs
│   ├── parser.rs         # Parse files, run queries, cache invalidation
│   ├── languages.rs      # Grammar loading (Rust, Python, TS)
│   └── queries/          # .scm query files per language
│       ├── rust.scm
│       ├── python.scm
│       └── typescript.scm
├── agent/
│   ├── mod.rs
│   ├── loop.rs           # Core agent loop: prompt → LLM → tool → repeat
│   ├── context.rs        # System prompt construction
│   └── budget.rs         # Token budget tracking, context window management
└── stream/
    ├── mod.rs
    └── output.rs         # JSON-lines, text, stream-json formatters
```

## CLI Interface

```
carv [OPTIONS] [PROMPT]

Arguments:
  [PROMPT]                      Task prompt (reads stdin if piped)

Options:
  -m, --model <MODEL>           Model name (auto-detects provider)
  --provider <PROVIDER>         Explicit provider override: anthropic | openai
  -p, --print                   Non-interactive output mode
  --max-turns <N>               Max tool-use rounds (default: 50)
  --output-format <FORMAT>      text | json | stream-json (default: text)
  --system-prompt <PROMPT>      Custom system prompt
  --disallowed-tools <TOOLS>    Comma-separated tools to disable
  -v, --verbose                 Debug output to stderr
```

API keys are read from environment variables only (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`). No `--api-key` CLI flag — passing secrets as arguments leaks them to shell history and `/proc`.

Provider auto-detection from model name (overridden by `--provider`):
- `claude-*`, `anthropic/*` → Anthropic
- `gpt-*`, `chatgpt-*`, `o1-*`, `o3-*`, `o4-*` → OpenAI
- Unknown model name → error unless `--provider` is set

## LLM Provider

```rust
pub type LlmStream = Pin<Box<dyn Stream<Item = Result<LlmEvent>> + Send>>;

pub type LlmStreamFuture<'a> = Pin<Box<dyn Future<Output = Result<LlmStream>> + Send + 'a>>;

pub trait LlmProvider: Send + Sync {
    fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &RequestConfig,
    ) -> LlmStreamFuture<'_>;
}
```

No `async-trait` crate — uses explicit boxed futures for object safety (so `Arc<dyn LlmProvider>` works in the agent loop). Same pattern as `StreamOutput`.

### RequestConfig

```rust
pub struct RequestConfig {
    pub max_tokens: u32,              // required by Anthropic
    pub temperature: Option<f32>,     // default 0.0 for deterministic edits
    pub top_p: Option<f32>,
    pub stop_sequences: Vec<String>,  // optional stop sequences
    pub thinking: bool,               // enable extended thinking / reasoning
    pub thinking_budget: Option<u32>, // budget_tokens for Claude thinking
}
```

### StreamEvent

```rust
pub enum StreamEvent {
    Text(String),
    Thinking(String),              // Claude thinking blocks / OpenAI reasoning
    ToolUseDelta { id: String, name: Option<String>, input_json: String },
    ToolUseComplete { id: String, name: String, input: Value },
    Done { usage: Option<Usage> },
    Error(String),
}
```

Both providers must set `stream: true` on their wire requests. Tool calls arrive as streaming deltas (`ToolUseDelta`) with partial JSON fragments. The agent loop accumulates fragments per tool-call ID and emits `ToolUseComplete` once the block finishes. This matches both Anthropic's `input_json_delta` and OpenAI's `function.arguments` delta patterns.

### Prompt Caching (Anthropic)

The Anthropic provider annotates system prompt and tool definition blocks with `cache_control: {"type": "ephemeral"}` to enable prompt caching. This is critical for multi-turn coding agents — without it, re-sending the same system prompt and 12 tool definitions every turn costs 10-25x more.

The `Message` type includes an optional `cache_control` field:
```rust
pub struct ContentBlock {
    pub content: ContentType,
    pub cache_control: Option<CacheControl>,
}
```

### Tool Schema Mapping

The internal `ToolDef` uses a provider-agnostic format. Each provider serializes it to its wire format:
- **Anthropic:** `{"name", "description", "input_schema"}`
- **OpenAI:** `{"type": "function", "function": {"name", "description", "parameters"}}`

### Token Usage Tracking

Each `Done` event includes a `Usage` struct:
```rust
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
}
```

The agent loop accumulates usage across turns and reports totals in the final output.

## Tools

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;  // JSON Schema
    fn is_read_only(&self) -> bool;        // informational, used in verbose output
    fn execute<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> ToolFuture<'a>;  // boxed future for object safety (same pattern as LlmProvider)
}
```

The boxed-future return type (`ToolFuture<'a>`) rather than RPITIT `impl Future` keeps the trait **object-safe**, so callers can hold `dyn Tool` in a registry (`Vec<Box<dyn Tool>>`). Same pattern as `LlmProvider`.

All tools auto-approved. `--disallowed-tools` removes tools from the registry before the LLM sees them. `is_read_only()` is informational — shown in `--verbose` output and stream-json events, but does not gate execution.

### Tool Inventory

| Tool | Read-only | Description |
|---|---|---|
| `read_file` | yes | Read file with hash-anchored lines |
| `write_file` | no | Write/create file (new files or full overwrite) |
| `edit_file` | no | Hash-anchored edits: replace/insert_before/insert_after. Supports multi-file batching. |
| `list_files` | yes | List directory contents (.gitignore-aware) |
| `search_files` | yes | Ripgrep content search, .gitignore-aware (returns hash-anchored lines) |
| `execute_command` | no | Run shell command (sandboxed: 30s timeout, cwd pinned, output capped) |
| `get_skeleton` | yes | AST structural outline (hash-anchored) |
| `get_function` | yes | Extract function body by name (hash-anchored) |
| `replace_symbol` | no | Replace function/class by AST node. Supports multi-file batching. |
| `lsp_rename` | no | Semantic rename across project |
| `lsp_references` | yes | Find all references |
| `lsp_definition` | yes | Go to definition |
| `lsp_diagnostics` | yes | Current type errors/warnings |

### execute_command Sandboxing

- **Timeout:** Configurable, default 30 seconds. Kills the child process on expiry.
- **Working directory:** Pinned to project root. Cannot escape via `cd`.
- **Output cap:** Stdout/stderr truncated to 32KB. Overflow noted in tool result.
- **No shell interpretation:** Commands run via `tokio::process::Command`, not through `sh -c` (avoids injection). Arguments are passed as a vector.

## Hash-Anchored Editing

Every line returned by read tools (`read_file`, `get_function`, `get_skeleton`, `search_files`) is prefixed with a stable word-based anchor:

```
Apple│    def process(param1, param2):
Brave│        total = 0
Cider│        for item in items:
```

### Anchor Generation

Anchors are derived from a hash of line content, mapped to a human-readable word from a fixed dictionary. They remain stable across insertions/deletions elsewhere in the file, unlike line numbers.

**Duplicate-line disambiguation:** Identical lines (blank lines, `}`, `pass`, repeated patterns) produce the same hash. To disambiguate, an occurrence index is appended when a collision is detected: `Delta│}`, `Delta.1│}`, `Delta.2│}`. The anchor `Delta` always refers to the first occurrence; `Delta.1` refers to the second, etc. The `hashing/state.rs` module tracks these per-file.

### edit_file

The `edit_file` tool uses anchors to target edits precisely:

- **`replace`** — replace an inclusive range from `anchor` to `end_anchor` with new text
- **`insert_after`** — insert new lines after `anchor`
- **`insert_before`** — insert new lines before `anchor`

### Multi-file Batching

`edit_file` accepts a `files` array, allowing edits across multiple files in a single LLM tool call:

```json
{
  "files": [
    { "path": "src/foo.rs", "edits": [{ "edit_type": "replace", "anchor": "...", "end_anchor": "...", "text": "..." }] },
    { "path": "src/bar.rs", "edits": [{ "edit_type": "insert_after", "anchor": "...", "text": "..." }] }
  ]
}
```

Multiple edits in the same file must be non-overlapping and are applied bottom-to-top to preserve anchor validity.

### Anchor & AST Cache Invalidation

The `hashing/state.rs` module maintains per-file anchor mappings for the duration of a session. When a file is modified (by `edit_file`, `write_file`, or `replace_symbol`):

1. Anchor mappings for the file are recomputed
2. The tree-sitter parse tree cache for the file is invalidated (forces re-parse on next `get_skeleton`/`get_function`/`replace_symbol`)
3. LSP is notified via `textDocument/didChange`

## Tree-sitter Integration

Native C bindings via `tree-sitter` crate (not WASM). Grammars compiled at build time via `cc` in `[build-dependencies]`.

**Crates:** All pinned to the same major version to avoid ABI mismatch. Check actual latest compatible versions on crates.io before implementation — `tree-sitter-typescript` may lag behind the core crate.

**Query files:** Embedded via `include_str!()`. Each language query captures:
- `@definition.function`, `@definition.method`, `@definition.class`, `@definition.interface`
- `@name.definition.*` for the identifier within each definition
- `@name.reference` for all identifier references

**Parse tree caching:** Parsed trees are cached per-file. Cache is invalidated when any tool modifies the file (see Anchor & AST Cache Invalidation above).

**get_skeleton:** Parse → run definition query → return only signature lines with their line numbers.

**get_function:** Find named symbol via dot-path (`Class.method`), walk parent nodes for full qualified name, extract byte range, return body.

**replace_symbol:** Find symbol's AST node byte range (extending to include decorators/comments/export wrappers), splice in new code. Multiple replacements applied bottom-to-top to avoid offset corruption.

**Cross-compilation note:** Compiling C grammars requires a C compiler. `cc` crate handles this on most platforms. musl targets and cross-compilation may need extra flags — document in CONTRIBUTING.md.

## LSP Integration

### Client Lifecycle

1. Tool invoked for a file → registry checks extension → finds server config
2. If server not running → spawn via `tokio::process::Command`, send `initialize`/`initialized`
3. `textDocument/didOpen` for the target file
4. Execute the request (`rename`, `references`, `definition`)
5. After any file modification, send `textDocument/didChange` and **await server readiness** before returning LSP results (see Synchronization below)
6. Servers kept alive for session duration, killed on `carv` exit

### Stdio Handle Ownership

LSP servers communicate over stdin/stdout. The client must:
1. Take ownership of `child.stdin` and `child.stdout` via `.take()`
2. Wrap stdin in a write half, stdout in a read half
3. Use a channel-based architecture: outbound requests queue to a stdin writer task, inbound responses/notifications are dispatched from a stdout reader task

This avoids borrow-of-moved-value issues with concurrent read/write on the child process.

### Open File Tracking

The LSP client maintains a `HashSet<PathBuf>` of files that have been sent `textDocument/didOpen`. This is used to:
- Avoid duplicate `didOpen` notifications (which violate the LSP spec)
- Re-open all tracked files after a server crash/restart
- Send `textDocument/didClose` during graceful shutdown

### Synchronization

After sending `textDocument/didChange`, the server needs time to process the update. Before returning results from `lsp_diagnostics`, `lsp_references`, or `lsp_rename`:
- Wait for a fresh `textDocument/publishDiagnostics` notification, or
- Send a follow-up request and use its response as a synchronization barrier

This prevents stale results after file modifications.

### Auto-detection

Scan project root for markers:
- `Cargo.toml` → Rust → `rust-analyzer`
- `pyproject.toml` / `setup.py` / `requirements.txt` → Python → `ty server`
- `tsconfig.json` / `package.json` → TypeScript → `typescript-language-server --stdio`

### Lazy Startup

Servers spawned on first LSP tool use for that language, not at init.

### Server Crash Recovery

On LSP server crash:
1. Log warning to stderr
2. Attempt **one restart** (re-spawn, re-initialize, re-open all files tracked in the open file set)
3. If the restart fails, mark that language's LSP tools as unavailable for the rest of the session
4. Continue with tree-sitter-only operations for that language

### Graceful Shutdown

On `carv` exit, each running LSP server receives the proper shutdown sequence:
1. Send `shutdown` request and await response
2. Send `exit` notification
3. Wait briefly for the process to terminate, then kill if unresponsive

Raw killing without `shutdown` can corrupt server-side caches (e.g., rust-analyzer's proc-macro cache).

### Server Config (hardcoded for MVP)

| Language | Markers | Server | Command |
|---|---|---|---|
| Rust | `Cargo.toml` | rust-analyzer | `rust-analyzer` |
| Python | `pyproject.toml`, `setup.py` | ty | `ty server` |
| TypeScript | `tsconfig.json`, `package.json` | typescript-language-server | `typescript-language-server --stdio` |

## Streaming Output

### stream-json format (default for programmatic use)
```jsonl
{"type":"text","content":"I'll rename the function."}
{"type":"thinking","content":"Let me check what files reference getData..."}
{"type":"tool_use","id":"t1","name":"lsp_rename","input":{"symbol":"getData","new_name":"fetch_data","paths":["src/"]}}
{"type":"tool_result","id":"t1","content":"Renamed 12 occurrences across 4 files"}
{"type":"text","content":"Done."}
{"type":"done","turns":2,"usage":{"input_tokens":1240,"output_tokens":380,"cache_read_tokens":890}}
```

### text format (default for terminal)
Plain text content only, tool calls shown as `[tool: name]` lines with `--verbose`.

### json format
Single JSON object with full conversation after completion.

## Agent Loop

```
1. Build messages: system prompt (cached) + tools (cached) + (piped stdin as context) + user prompt
2. Call provider.stream_chat(messages, tools, config)
3. For each StreamEvent:
   - Text → stream to output
   - Thinking → stream to output (if verbose) or discard
   - ToolUseDelta → accumulate JSON fragments per tool-call ID
   - ToolUseComplete → dispatch to registry → get ToolResult → append to messages
   - Done → record usage, exit loop
   - Error → retry up to 3x with backoff (respecting retry-after headers), then fail
4. If tool was executed, check token budget → if over threshold, compact/summarize → go to step 2
5. Stop on Done or max_turns reached
6. Emit final summary: total turns, total tokens, total cost estimate
```

### Token Budget Management

The `agent/budget.rs` module tracks cumulative token usage across turns. Before each LLM call:
1. Estimate the current message payload size (system prompt + conversation history + tool results)
2. If approaching the model's context window (80% threshold), truncate old tool results — keep the last N tool results in full, summarize older ones to a single line each
3. If the context is still too large, drop the oldest conversation turns entirely

This prevents blowing the context window on long sessions with large tool outputs.

## Error Handling

- **LLM API errors:** Retry with exponential backoff (3 attempts), respecting `retry-after` headers on 429 responses and `529` (Anthropic overloaded). Emit error event and exit on exhaustion.
- **Tool errors:** Return error as ToolResult string to the LLM (let it recover/retry with a different approach)
- **LSP server crash:** Attempt one restart. If restart fails, mark language's LSP tools as unavailable, continue with tree-sitter-only.
- **Tree-sitter parse failure:** Fall back to raw read_file/write_file for that file
- **No panics in the agent loop:** All errors are `Result<T>`

## Dependencies

```toml
[dependencies]
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-util", "process", "time", "fs", "sync"] }
reqwest = { version = "0.12", features = ["stream"] }
reqwest-eventsource = "0.6"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
futures = "0.3"
tree-sitter = "0.24"
tree-sitter-rust = "0.24"
tree-sitter-python = "0.24"
tree-sitter-typescript = "0.24"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
ignore = "0.4"                   # .gitignore-aware file walking
grep-regex = "*"                 # ripgrep regex engine (verify latest on crates.io)
grep-searcher = "*"              # ripgrep file searcher (verify latest on crates.io)

[build-dependencies]
cc = "1"                         # compile tree-sitter C grammars
```

**Removed:** `async-trait` (native async fn in traits since Rust 1.75), `eventsource-stream` (replaced by `reqwest-eventsource`).

**Note:** Pin all `tree-sitter-*` grammar crates to the same major version as the `tree-sitter` core crate. Verify actual latest compatible versions on crates.io before implementation — grammar crates sometimes lag behind.

## Testing & Verification

### Unit Tests
- Each tool handler with mock file system and mock inputs
- Anchor generation and collision disambiguation
- StreamEvent parsing for both Anthropic and OpenAI wire formats
- Token budget calculations and truncation logic
- Tool schema serialization for both provider formats

### Integration Tests
- **Agent loop:** Mock LLM provider + mock tools to verify retry logic, max_turns, error propagation, and token budget enforcement with deterministic scenarios
- Run carv against a small fixture project with known structure, verify output format
- Tree-sitter tests: parse known Rust/Python/TS files, verify skeleton and function extraction

### LSP Tests
- Spawn real language servers against small fixture projects
- Verify didChange synchronization (edit → diagnostics → no stale results)
- Verify crash recovery (kill server mid-session → restart → resume)

### Smoke test after scaffold
```bash
cargo build
cargo test
echo "hello" | carv -p -m claude-sonnet-4-20250514 "what is this?"
carv -p -m gpt-4o "list files in src/"
```
