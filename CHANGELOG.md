# Changelog

All notable changes to carv are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- LSP integration — semantic rename, references, definition, diagnostics via rust-analyzer,
  `ty` server, and `typescript-language-server` with lazy lifecycle and crash recovery
  (see [design doc](docs/designs/2026-04-25-carv-design.md#lsp-integration)).
- LSP tools (`lsp_rename`, `lsp_references`, `lsp_definition`, `lsp_diagnostics`) in the tool registry.
- GitHub Actions CI pipeline (build, test, clippy, fmt).

## [0.1.0] — 2026-06-19

### Milestone 1: Foundation
*Initial project scaffold, CLI parsing, and hash-anchored line referencing.*

- Project skeleton with Cargo.toml, module tree, and dependency setup.
- `CarvArgs` CLI argument struct with clap derive (`--model`, `--provider`, `--output-format`,
  `--max-turns`, `--verbose`, etc.).
- `CarvConfig` struct with provider auto-detection from model names.
- API key loading from environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`).
- Word-based anchor generation using FNV-1a hash and a fixed dictionary.
- Per-file `AnchorState` manager with occurrence-index collision disambiguation for
  duplicate lines (e.g., `Delta`, `Delta.1`, `Delta.2`).

### Milestone 2: Providers + Tool System
*Dual LLM provider support (Anthropic + OpenAI) with SSE streaming and extensible tool system.*

- `LlmProvider` trait with boxed futures (`LlmStreamFuture`) for object safety — no
  `async-trait` crate dependency.
- Wire types shared across providers: `Message`, `ToolDef`, `StreamEvent` (with delta
  accumulation), `RequestConfig`, `Usage`.
- `StreamOutput` trait with three formatters: plain text, JSON, and JSON-lines (stream-json).
- **Anthropic provider:** `/v1/messages` SSE streaming with tool-use delta accumulation,
  prompt caching via `cache_control` annotations, and retry/backoff for 429/529 errors.
- **OpenAI provider:** `/v1/chat/completions` SSE streaming with reasoning tokens,
  tool-call completion ordering, and retry/backoff.
- `Tool` trait with JSON Schema parameter definitions, `ToolRegistry` with deny-list
  filtering, `ToolContext`, and `ToolResult` types.

### Milestone 3: Basic Tools
*Filesystem, search, and command execution tools with sandboxing.*

- `read_file` — reads files with hash-anchored line references for stable editing.
- `write_file` — writes or creates files (full overwrite).
- `list_files` — directory listing with `.gitignore` awareness (`ignore` crate).
- `search_files` — ripgrep-based content search with `.gitignore` awareness and
  hash-anchored output lines (`grep-regex` + `grep-searcher`).
- `execute_command` — sandboxed command execution with 30s timeout, pinned working
  directory, 32KB output cap, and no shell interpolation (`tokio::process::Command`).
- `edit_file` — hash-anchored editing supporting `replace`, `insert_before`, and
  `insert_after` operations with multi-file batching (edits applied bottom-to-top).
- Pre-commit hooks via `cargo-husky` with `cargo fmt`, `clippy`, `test`, and
  `git-secrets` scanning.

### Milestone 4: Tree-sitter Integration
*AST-level structural tools for Rust, Python, and TypeScript.*

- Tree-sitter core crate (`tree-sitter = "0.26"`) with grammar crates for Rust,
  Python, and TypeScript.
- Language loading and grammar registry with runtime language detection.
- Parse tree caching with invalidation on file modification.
- S-expression query (`.scm`) files for each language capturing definitions,
  references, and identifiers.
- `get_skeleton` — AST structural outline with hash-anchored signatures.
- `get_function` — extract function/method body by dot-path name.
- `replace_symbol` — replace function or class by AST node with multi-file batching
  (applied bottom-to-top to avoid offset corruption).

### Milestone 5: Agent Orchestration
*Core agent loop, token budget management, and main entrypoint wiring.*

- System prompt construction with tool descriptions and guidelines.
- Token budget tracking with per-turn accumulation and 80% context window threshold
  for compaction.
- Core agent loop: build messages → stream LLM response → accumulate tool deltas →
  dispatch tool → append result → repeat until done or max turns reached.
- Error handling: 3 retries with exponential backoff for LLM API errors, tool errors
  returned as strings to the LLM for recovery.
- Main entrypoint wiring: `CliArgs::parse()` → provider factory → tool registry build →
  agent loop → stream output.
- `cargo-deny` CI configuration for vulnerability and notice advisory checking.
- Documentation: `AGENTS.md` (AI agent guide), `README.md`, design doc, contributor
  guidelines, and changelog.

[unreleased]: https://github.com/decko/carv/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/decko/carv/releases/tag/v0.1.0
