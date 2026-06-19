# Contributing to carv

Thank you for your interest in contributing to **carv**, a minimal Rust coding agent with Tree-sitter and LSP integration.

This document covers everything you need to know to contribute effectively.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Workflow](#development-workflow)
- [Code Style](#code-style)
- [Testing Requirements](#testing-requirements)
- [Pull Request Process](#pull-request-process)
- [Issue Tracking](#issue-tracking)
- [Architecture Overview](#architecture-overview)

---

## Getting Started

### Prerequisites

- **Rust 1.80+** (edition 2021). Install via [rustup](https://rustup.rs/).
- **C compiler** — required by Tree-sitter grammar crates (C source compiled at build time). On most platforms this is already present (`gcc`, `clang`, or MSVC).
- **git-secrets** (optional, recommended) — scans for accidentally committed secrets.

### Clone and Build

```bash
git clone https://github.com/decko/carv.git
cd carv
cargo build
cargo test
```

### Running

```bash
# Basic usage
cargo run -- "list files in src/"

# With a specific model
cargo run -- -m claude-sonnet-4-20250514 "explain src/main.rs"

# Non-interactive mode
cargo run -- -p "refactor this function"

# See all options
cargo run -- --help
```

API keys are read from environment variables only:
- `ANTHROPIC_API_KEY` for Claude models
- `OPENAI_API_KEY` for GPT models

Never pass API keys as CLI arguments — they leak through shell history and `/proc`.

---

## Development Workflow

### Git Worktrees

All development happens in **isolated git worktrees**, never directly on `main` or any named branch.

```bash
# Create a worktree for a new task
git worktree add -b task/<issue-number>-<short-slug> .worktrees/task/<slug> main

# Work inside the worktree
cd .worktrees/task/<slug>

# After the PR is merged, clean up
git worktree remove .worktrees/task/<slug>
git branch -D task/<issue-number>-<short-slug>
```

This keeps the main checkout pristine — if something goes wrong (bad edits, corrupted state), other tasks are unaffected.

### Branch Naming Convention

```
task/<github-issue-number>-<short-slug>
```

Examples: `task/42-add-lsp-crash-recovery`, `task/7-llm-retry-logic`

### SSH Commit Signing

Every commit must be SSH-signed. Configure signing in the repository's local git config:

```bash
git config --local gpg.format ssh
git config --local user.signingkey "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIINgVXr/ijCjWvgKFW5mlCIG8Njjkoq3ptCzD/VicJ39 decko@lion"
git config --local commit.gpgsign true
```

The private key is held in the SSH agent (Bitwarden). Verify signing is active:

```bash
git config --local commit.gpgsign   # must return "true"
```

> **Note:** `git log --show-signature` may print "No signature" when `gpg.ssh.allowedSignersFile` is not configured — even though the commit IS signed. This is a local verification issue, not a signing failure. GitHub verifies SSH signatures natively.

### Pre-commit Hooks

Client-side git hooks are managed by [cargo-husky](https://github.com/rhsyd/cargo-husky). Hooks are auto-installed into `.git/hooks/` on `cargo test` and run on every `git commit`:

| Hook | What it does |
|---|---|
| `cargo fmt -- --check` | Rejects unformatted Rust code |
| `cargo clippy -- -D warnings` | Rejects clippy warnings |
| `cargo test` | Rejects if any test fails |
| `git secrets --scan --cached` | Scans staged files for API keys and credentials |

#### Manual Installation (Worktrees)

cargo-husky does not fully support git worktrees. If you work in a worktree, install the hook manually:

```bash
WORKTREE_GITDIR=$(cat .git | sed 's/gitdir: //')/hooks
mkdir -p "$WORKTREE_GITDIR"
cp .cargo-husky/hooks/pre-commit "$WORKTREE_GITDIR/pre-commit"
chmod +x "$WORKTREE_GITDIR/pre-commit"
```

#### Secret Scanning

Install [git-secrets](https://github.com/awslabs/git-secrets) for automatic secret detection:

```bash
# macOS
brew install git-secrets

# Linux (from source)
git clone https://github.com/awslabs/git-secrets.git
cd git-secrets && sudo make install
```

If git-secrets is not installed, the pre-commit hook skips secret scanning (non-blocking).

#### Bypassing Hooks

In emergencies, skip hooks with:

```bash
git commit --no-verify -m "message"
```

### Cargo.lock Policy

Commit `Cargo.lock`. carv is a binary crate — lockfiles ensure reproducible builds. (Library crates typically omit them; binaries do not.)

---

## Code Style

- **Rust edition 2021** with minimum Rust version 1.80.
- **CLI:** `clap` derive for argument parsing. Define structs with `#[derive(Parser)]`.
- **Wire types:** `serde` derive for serialization (`#[derive(Serialize, Deserialize)]`).
- **Logging:** `tracing` + `tracing-subscriber` for structured logging. No `println!`, `dbg!`, or `eprintln!` in production code.
- **Naming:** The project prefix is `Carv` (not `Carve`). The crate is `carv`. Use `CarvArgs`, `CarvConfig`, `CarvError`, etc.
- **Error handling:**
  - `anyhow` for application-level errors (agent loop, CLI).
  - `thiserror` for library-level errors (LLM provider, LSP transport, tree-sitter module).
  - Tool errors are returned as strings to the LLM (it can retry or recover).
  - No panics in the agent loop — every code path returns `Result<T>`.
- **Async:** Native `async fn` in traits (RPITIT, Rust 1.75+). No `async-trait` crate. Traits requiring object safety use `Pin<Box<dyn Future>>` type aliases.
- **Stream results** via `Pin<Box<dyn Stream<Item = Result<T>> + Send>>`.
- Functions used only in `#[cfg(test)]` should be annotated with `#[allow(dead_code)]` (with a comment explaining why), since clippy's `dead_code` lint does not count test code as usage.

### Critical Invariants

1. **No `async-trait` crate** — use native async fn in traits.
2. **No panics in the agent loop** — every code path returns `Result<T>`.
3. **Hash-anchored line referencing** — every line read tool returns stable word-based anchors, not line numbers. `edit_file` only accepts anchors.
4. **Multi-file batching** — `edit_file` and `replace_symbol` accept a `files` array with all edits applied bottom-to-top.
5. **LSP lifecycle** — servers are spawned lazily (first use), receive graceful shutdown on exit, with one restart attempt on crash.
6. **Sandboxed execution** — `execute_command` has a 30s timeout, pinned cwd, 32KB output cap, no shell interpolation.
7. **API keys are env-only** — `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`, never CLI args.
8. **Token budget tracking** — context window management trims old tool results at 80% window capacity.

---

## Testing Requirements

Run these commands before every commit (they run automatically via pre-commit hooks):

```bash
cargo build           # Full build
cargo test            # All tests green
cargo clippy -- -D warnings   # Zero warnings
cargo fmt -- --check  # Properly formatted
```

### Testing Strategy

- **Unit tests** — per-tool handler with mock file system and inputs, anchor generation, wire format parsing.
- **Integration tests** — agent loop with mock LLM + mock tools, fixture projects.
- **LSP tests** — real language servers against fixture projects (spawn, sync, crash recovery).

### Tree-sitter Query File Review

Query files (`.scm`) are S-expression patterns that match AST node types. Review them by:
- Checking that `@definition.*` captures match the correct node types for each grammar.
- Verifying that `@name.*` captures reference the right child nodes within definitions.
- Testing against fixture files to confirm captures fire correctly.

These are a distinct review category from Rust code — review them like templates against a known grammar, not like program logic.

---

## Pull Request Process

### Before Submitting

1. Ensure all tests pass and clippy is clean.
2. Update the design doc (`docs/designs/2026-04-25-carv-design.md`) if your changes affect the architecture or public interface.
3. Add tests for all new code (unit tests for tools, integration tests for flows, self-tests for carv itself).
4. No new dependencies without explicit justification. If adding a dependency, explain why in the PR description.
5. No public API changes without explicit justification.
6. No security boundary modifications (sandbox configs, timeouts, command execution, LSP protocol contracts) without explicit justification.
7. If changing a constant, threshold, word count, or numeric parameter, grep for related comments across the crate — stale comments mislead future maintainers.
8. Keep PRs focused and reasonably sized (ideally under 300 lines of diff).

### PR Description Template

```markdown
## Summary
[1-2 lines describing the change]

## Changes
- [Detailed list of changes]

## Testing
- [How this was tested]

Closes #[issue-number]
```

### Review Process

1. The PR author runs the full verification suite (`cargo build`, `test`, `clippy`, `fmt`).
2. A reviewer checks the Definition of Done checklist (see below).
3. The reviewer either approves or requests changes.
4. Once approved, the PR is merged (squash merge preferred).

### Definition of Done Checklist

For every PR:

- [ ] `cargo build` passes with no errors
- [ ] `cargo test` passes (all tests green)
- [ ] `cargo clippy -- -D warnings` passes with no warnings
- [ ] `cargo fmt -- --check` passes
- [ ] No new dependencies added (or justified with approval)
- [ ] No public API signatures changed (or justified with approval)
- [ ] No security boundary modified
- [ ] Design doc is still consistent with changes (updated if needed)
- [ ] All new code has tests
- [ ] Error handling follows the project philosophy (no panics in loop, tool errors returned as strings to LLM)

### Review Depth Standards

Reviewers should go beyond mechanical checks. Key items to flag:

- **For all PRs:** Redundant `#[serde(default)]` on `Option<T>` fields, missing `PartialEq`/`Debug` derives on types used in tests, `#[rustfmt::skip]` without a comment, every public type/function should have a covering test.
- **For serde-heavy PRs:** Verify every `#[serde(rename)]` matches the actual wire-level key in the provider's API reference. Verify one deserialization test per tagged-enum variant. Test round-trip with absent optional fields.
- **For security-sensitive PRs:** Command execution must use `env_clear()` or curated env (no API key inheritance). `tokio::spawn()` handles must be awaited or aborted on all code paths. Timeout must `start_kill()` + `wait().await` — no zombie processes. No shell injection — use `Command::new(cmd).args(args)`, never `sh -c` with string interpolation.

---

## Issue Tracking

- All work is tracked in [GitHub Issues](https://github.com/decko/carv/issues).
- Each issue should represent a single, focused piece of work.
- Issues are assigned to the project owner (`decko`) when work begins.
- Each PR closes one or more issues.
- Issues follow the format:
  - **Context** — what's the problem or motivation
  - **Requirements** / **Acceptance Criteria** — what done looks like
  - **DoD Checklist** — the Definition of Done checklist items

---

## Architecture Overview

**carv** is a single Rust binary (monolith). Key modules:

| Module | Responsibility |
|---|---|
| `cli/` | Clap derive argument parsing, config loading |
| `llm/` | Dual provider trait (Anthropic SSE + OpenAI SSE), native async fn |
| `tools/` | Tool registry with deny-list filtering, auto-approved execution |
| `lsp/` | JSON-RPC over stdio, lazy language server lifecycle, crash recovery |
| `treesitter/` | Grammar bindings, `.scm` query files, parse tree caching |
| `hashing/` | Word-based stable anchors with duplicate-line disambiguation |
| `agent/` | Core loop: prompt → LLM → tool → repeat, token budget tracking |
| `stream/` | JSONL, text, and stream-json output formatters |

For the full design specification, see [docs/designs/2026-04-25-carv-design.md](docs/designs/2026-04-25-carv-design.md).

### Key Dependencies

- `tokio` — multi-threaded runtime, process spawning, channels
- `tree-sitter` + grammars (Rust, Python, TypeScript) — AST parsing
- `reqwest` + `reqwest-eventsource` — SSE streaming for LLM providers
- `ignore` — .gitignore-aware file walking
- `grep-regex` + `grep-searcher` — ripgrep engine for `search_files`
- `serde` / `serde_json` — serialization

---

## License

carv is licensed under the MIT license. By contributing, you agree that your contributions will be licensed under the same license.
