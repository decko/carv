# carv

**Minimal Rust Coding Agent — Tree-sitter structure + LSP semantics + Dual LLM providers**

[![Crates.io](https://img.shields.io/crates/v/carv.svg)](https://crates.io/crates/carv)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://rust-lang.org)

carv is a standalone, non-interactive Rust CLI coding agent that combines
**tree-sitter structural understanding** (AST-level tools) with **LSP semantic
intelligence** (scope-aware renames, type diagnostics, cross-file references).
It streams output in text, JSON, or JSON-lines format and supports both
Anthropic (Claude) and OpenAI (GPT) as LLM backends.

---

## Features

- **Hash-anchored editing** — Stable word-based anchors instead of line numbers.
  Anchors survive insertions/deletions elsewhere in the file. Duplicate lines
  (blank lines, `}`, `pass`) get occurrence-index disambiguation.
- **Tree-sitter integration** — AST-aware tools for Rust, Python, and
  TypeScript: `get_skeleton` (structural outline), `get_function` (body
  extraction), `replace_symbol` (AST-safe replacement with decorator/comment
  boundary awareness).
- **Multi-file batching** — `edit_file` and `replace_symbol` accept a `files`
  array. Multiple edits in a single LLM tool call are applied bottom-to-top to
  preserve anchor validity.
- **Dual LLM providers** — Anthropic SSE (`/v1/messages`) with prompt caching
  and extended thinking. OpenAI SSE (`/v1/chat/completions`). Provider
  auto-detection from model name prefix.
- **Token budget tracking** — Context window managed at 80% threshold. Old tool
  results are compacted before the window is exceeded. Usage statistics reported
  per run.
- **Streaming output** — Three formatters: plain text (terminal), single JSON
  object (completion), JSON-lines stream (programmatic consumption).
- **Resource-limited execution** — `execute_command` has a 30-second timeout,
  pinned working directory, 32KB output cap, and no shell interpolation (args
  passed as vector).
- **LSP integration** *(planned)* — Lazily spawned language servers for Rust
  (`rust-analyzer`), Python (`ty`), and TypeScript
  (`typescript-language-server`). Graceful shutdown, crash recovery with one
  restart attempt, and automatic re-opening of tracked files after restart.
- **Retry logic** — LLM API errors retry 3× with exponential backoff, respecting
  `retry-after` headers.
- **Piped stdin** — Accept prompts via pipe for `git diff | carv "review"` workflows.

## Requirements

- **Rust 1.80+** — required for native async fn in traits (RPITIT).
- **API key** for at least one provider, set as an environment variable:
  - `ANTHROPIC_API_KEY` for Anthropic/Claude models
  - `OPENAI_API_KEY` for OpenAI/GPT models
- **Optional — Language servers** for planned LSP features:
  - `rust-analyzer` (Rust)
  - `ty server` (Python)
  - `typescript-language-server` (TypeScript)

## Installation

```bash
cargo install carv
```

Or build from source:

```bash
git clone https://github.com/decko/carv.git
cd carv
cargo build --release
```

## Usage

### Basic prompt

```bash
carv "explain the architecture in src/agent/loop.rs"
```

### Specify a model

Provider is auto-detected from the model name prefix:

```bash
carv -m claude-sonnet-4-20250514 "refactor this module"
carv -m gpt-4o "list files in src/"
carv -m o3-mini "analyze this code"
```

### Explicit provider override

```bash
carv -m gpt-4o --provider anthropic "explain this code"
```

### Pipe from git

```bash
git diff | carv "review these changes"
```

### Non-interactive mode (print result only)

```bash
carv -p -m claude-sonnet-4-20250514 "summarize src/main.rs"
```

### Programmatic output (JSON lines)

```bash
carv --output-format stream-json "analyze the API surface" > results.jsonl
```

### Restrict tools

```bash
carv --disallowed-tools execute_command,write_file "refactor safely"
```

## CLI Reference

```
carv [OPTIONS] [PROMPT]

Arguments:
  [PROMPT]                      Task prompt (reads stdin if piped)

Options:
  -m, --model <MODEL>           Model name (auto-detects provider)
  --provider <PROVIDER>         Provider override: anthropic | openai
  -p, --print                   Non-interactive output mode (tool-use hidden)
  --max-turns <N>               Maximum tool-use rounds [default: 50]
  --output-format <FORMAT>      Output format: text | json | stream-json [default: text]
  --system-prompt <PROMPT>      Custom system prompt (replaces default)
  --disallowed-tools <TOOLS>    Comma-separated list of tool names to disable
  -v, --verbose                 Enable debug output to stderr
  -h, --help                    Print help
  -V, --version                 Print version
```

### Provider auto-detection

| Model prefix            | Provider    |
|-------------------------|-------------|
| `claude-*`              | Anthropic   |
| `anthropic/*`           | Anthropic   |
| `gpt-*`                 | OpenAI      |
| `chatgpt-*`             | OpenAI      |
| `o1-*`, `o3-*`, `o4-*` | OpenAI      |

Unknown model prefixes require an explicit `--provider` flag.

### Output formats

| Format       | Description                                         |
|--------------|-----------------------------------------------------|
| `text`       | Plain text output, tool calls shown as `[tool: …]`  |
| `json`       | Single JSON object after completion                  |
| `stream-json`| JSON-lines stream (one event per line)               |

Stream-json event types: `text`, `thinking`, `tool_use`, `tool_result`, `done`.

## Architecture

carv is a single Rust binary (monolith). Key modules:

| Module       | Responsibility |
|--------------|----------------|
| `cli`        | Clap derive argument parsing, config resolution |
| `llm`        | Dual provider trait — Anthropic SSE + OpenAI SSE, native async fn |
| `tools`      | Tool registry with deny-list filtering, auto-approved execution |
| `agent`      | Core loop: prompt → LLM → tool → repeat, token budget tracking |
| `hashing`    | Word-based stable anchors, duplicate-line disambiguation |
| `treesitter` | AST parsing, query execution, parse tree caching |
| `lsp`        | JSON-RPC over stdio, lazy lifecycle, crash recovery *(planned)* |
| `stream`     | Text, JSON, and JSON-lines output formatters |

### Tools

| Tool                | Read-only | Description |
|---------------------|-----------|-------------|
| `read_file`         | ✓         | Read file with hash-anchored lines |
| `write_file`        |           | Write or create a file |
| `edit_file`         |           | Hash-anchored edits (replace/insert_before/insert_after), multi-file batching |
| `list_files`        | ✓         | List directory contents (.gitignore-aware) |
| `search_files`      | ✓         | Ripgrep content search with hash-anchored results |
| `execute_command`   |           | Run shell command (30s timeout, pinned cwd, 32KB output cap, no shell injection) |
| `get_skeleton`      | ✓         | AST structural outline (hash-anchored) |
| `get_function`      | ✓         | Extract function body by name |
| `replace_symbol`    |           | Replace function/class by AST node, multi-file batching |
| `lsp_rename`        |           | Semantic rename across the project *(planned)* |
| `lsp_references`    | ✓         | Find all references to a symbol *(planned)* |
| `lsp_definition`    | ✓         | Go to definition *(planned)* |
| `lsp_diagnostics`   | ✓         | Current type errors and warnings *(planned)* |

## Environment Variables

| Variable            | Required for | Description |
|---------------------|-------------|-------------|
| `ANTHROPIC_API_KEY` | Anthropic provider | API key for Claude models |
| `OPENAI_API_KEY`    | OpenAI provider    | API key for GPT models |

API keys are **never** accepted as CLI arguments. This prevents leakage through
shell history and `/proc`.

## Design

For a detailed design document covering architecture, data flow, and
implementation decisions, see:

[`docs/designs/2026-04-25-carv-design.md`](docs/designs/2026-04-25-carv-design.md)

## Contributing

See [`AGENTS.md`](AGENTS.md) for the project's development workflow,
including git conventions, pre-commit hooks, and the review process.

Key points for contributors:

- **No `async-trait` crate** — native async fn in traits (Rust 1.75+).
- **No panics in the agent loop** — all errors are `Result<T>`.
- **API keys are env-only** — never passed as CLI args.
- **SSH-signed commits** — every commit must be signed via ssh-agent.
- **Worktree-based development** — all work happens in isolated worktrees
  under `.worktrees/`. No direct commits to `main`.

### Pre-commit hooks

Pre-commit hooks are installed automatically via `cargo-husky` on `cargo test`:

- `cargo fmt -- --check` — rejects unformatted code
- `cargo clippy -- -D warnings` — rejects clippy warnings
- `cargo test` — rejects failing tests
- `git secrets --scan --cached` — scans for secrets

## License

Apache 2.0. See [LICENSE](LICENSE).
