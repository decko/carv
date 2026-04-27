# AGENTS.md — Project Guide for AI Coding Agents

## Build, Test, Lint

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

## Architecture

**carv** is a single Rust binary (monolith). Full spec: `docs/designs/2026-04-25-carv-design.md`

Key modules:
- `cli.rs` — clap derive argument parsing
- `llm/` — Dual provider trait (Anthropic SSE + OpenAI SSE), native async fn in traits
- `tools/` — Tool registry with deny-list filtering, auto-approved execution
- `lsp/` — JSON-RPC over stdio, lazy language server lifecycle, crash recovery
- `treesitter/` — C grammar bindings, .scm query files, parse tree caching
- `hashing/` — Word-based stable anchors with duplicate-line disambiguation
- `agent/` — Core loop: prompt → LLM → tool → repeat, token budget tracking
- `stream/` — JSONL, text, stream-json output formatters

## Critical Invariants

1. **No `async-trait` crate.** Use native async fn in traits (RPITIT, Rust 1.75+).
2. **No panics in the agent loop.** Every code path returns `Result<T>`.
3. **Hash-anchored line referencing.** Every line read tool returns stable word-based anchors, not line numbers. `edit_file` only accepts anchors.
4. **Multi-file batching.** `edit_file` and `replace_symbol` accept a `files` array — all edits in one LLM tool call, applied bottom-to-top.
5. **LSP lifecycle.** Servers are spawned lazily (first use), receive graceful shutdown on exit, with one restart attempt on crash.
6. **Sandboxed execution.** `execute_command` has a 30s timeout, pinned cwd, 32KB output cap, no shell interpolation.
7. **API keys are env-only.** `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` — never passed as CLI args.
8. **Token budget tracking.** Context window management trims old tool results at 80% window capacity.

## Git Workflow (MANDATORY)

### ⛔ NEVER commit directly to `main` or any named branch.

Commits to `main` are forbidden. Commits to named branches (e.g., `feature/*`, `fix/*`) are also forbidden. All work happens through the worktree workflow below.

### ✅ Always work in a git worktree under `.worktrees/`

Every task gets its own isolated worktree inside the `.worktrees/` directory (which is gitignored):

```bash
# 1. Create a worktree for the task (inside .worktrees/)
git worktree add -b task/<slug> .worktrees/task/<slug> main

# 2. Work inside the worktree (not the main checkout)
cd .worktrees/task/<slug>

# 3. Never commit inside the worktree — commits happen only through the
#    PR workflow below (the agent prepares changes, reviewer signs off,
#    then the PR is created from the worktree branch).
```

The `.worktrees/` directory must be listed in `.gitignore` to prevent accidental commits of worktree metadata.

**Rationale:** Worktrees isolate changes so the main checkout stays pristine. If an agent makes a mistake (bad edits, stale cache, corrupted state), the main checkout is untouched and other tasks are unaffected. Keeping worktrees under `.worktrees/` keeps them contained within the repo structure.

### ✅ Branch naming convention

```
task/<github-issue-number>-<short-slug>
```

Examples: `task/42-add-lsp-crash-recovery`, `task/7-llm-retry-logic`

### ✅ The worktree is temporary

After the PR is merged, the worktree branch is deleted and the worktree directory is cleaned up:

```bash
# After merge
git worktree remove .worktrees/task/<slug>
git branch -D task/<slug>
```

## Definition of Done (DoD) — Reviewer Gate

**Before generating a PR, always request a reviewer agent to verify the DoD checklist.** Do NOT proceed to PR creation until the reviewer signs off.

DoD checklist:

- [ ] `cargo build` passes with no errors
- [ ] `cargo test` passes (all tests green)
- [ ] `cargo clippy -- -D warnings` passes with no warnings
- [ ] `cargo fmt -- --check` passes
- [ ] No new dependencies added to `Cargo.toml` (or justified + explicitly approved)
- [ ] No public API signatures changed (or justified + explicitly approved)
- [ ] No security boundary modified — sandbox timeouts, command execution paths, LSP protocol contracts
- [ ] Design doc (`docs/designs/2026-04-25-carv-design.md`) is still consistent with the changes (update it if needed)
- [ ] All new code has tests (unit for tools, integration for flows, self-tests for carv itself)
- [ ] Error handling follows the project philosophy (no panics in loop, tool errors returned as strings to LLM)

**Reviewer handoff format:**

```
## PR Ready for DoD Review

**Branch:** task/<issue>-<slug>
**Summary:** [1-2 lines]

DoD checklist: [all items checked by agent, ready for reviewer verification]
```

**The reviewer responds with:**
- ✅ **Approved** — proceed to PR creation, OR
- ❌ **Changes needed** — list of failing items, agent fixes and resubmits

## Ticket Assignment

**Before starting any work, assign the GitHub issue to the project owner (`decko`).**

```bash
gh issue edit <issue-number> --add-assignee "decko"
```

If no GitHub issue exists yet, create one first:

```bash
gh issue create \
  --title "<title>" \
  --body "## Context\n\n## Acceptance Criteria\n\n## DoD Checklist\n- [ ] cargo build\n- [ ] cargo test\n- [ ] cargo clippy\n- [ ] cargo fmt\n- [ ] DoD review passed"
```

Then assign it before writing any code.

**Rationale:** The issue is the source of truth for what's being worked on. Assigning it to `decko` ensures visibility and prevents duplicate work.

## Complete Workflow Summary

```
1. Create or identify the GitHub issue → assign to decko
2. Create a git worktree under .worktrees/task/<slug>
3. Implement the changes inside the worktree
4. Run cargo build / test / clippy / fmt
5. Request reviewer agent to verify DoD
6. Reviewer approves → create PR from the worktree branch
7. After merge → clean up worktree and branch
```

## Error Handling Philosophy

- `anyhow` for application-level errors (the agent loop, CLI)
- `thiserror` for library-level errors (LLM provider, LSP transport, tree-sitter module)
- Tool errors are returned as `ToolResult` strings to the LLM (it can retry/recover)
- LLM API errors: 3 retries with exponential backoff, respecting `retry-after` headers
- LSP server crashes: one restart attempt, then mark language's LSP tools unavailable

## Code Style

- Rust edition 2021
- `clap` derive for CLI, `serde` derive for wire types
- `tracing` + `tracing-subscriber` for structured logging (not `println!`)
- Trait methods return `impl Future<Output = Result<T>> + Send` (no boxed futures)
- Stream results via `Pin<Box<dyn Stream<Item = Result<T>> + Send>>`
- Read-only vs. write tool distinction is informational only (shown in verbose/debug output)

## Testing Strategy

- Unit tests: per-tool handler with mock fs/inputs, anchor generation, wire format parsing
- Integration tests: agent loop with mock LLM + mock tools, fixture projects
- LSP tests: real language servers against fixture projects (spawn, sync, crash recovery)

## Dependencies to Know

- `tokio` — multi-threaded runtime, process spawning, channels
- `tree-sitter` + `tree-sitter-rust/python/typescript` — all pinned to same major version
- `reqwest` + `reqwest-eventsource` — SSE streaming for LLM providers
- `ignore` — .gitignore-aware file walking
- `grep-regex` + `grep-searcher` — ripgrep engine for `search_files`
- `cc` (build-dep) — compile C grammars at build time

## When Editing This Project

1. Check `docs/designs/2026-04-25-carv-design.md` for detailed requirements before implementation
2. Never change a public API (trait, struct field, function signature) without explicit approval
3. Never add a dependency to `Cargo.toml` without explicit approval
4. Never modify security boundaries (sandbox configs, timeouts, command execution) without explicit approval
5. After any file-modifying tool, invalidate anchor mappings and tree-sitter parse caches
