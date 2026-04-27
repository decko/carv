# AGENTS.md — Project Guide for AI Coding Agents

## Build, Test, Lint

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

**`Cargo.lock` policy:** Commit it. carv is a binary crate — lockfiles ensure reproducible builds. Library crates omit them; binaries do not.

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

### ✅ SSH Commit Signing

Every commit must be SSH-signed. The signing config lives in the repo's local git config:

```bash
git config --local gpg.format ssh
git config --local user.signingkey "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIINgVXr/ijCjWvgKFW5mlCIG8Njjkoq3ptCzD/VicJ39 decko@lion"
git config --local commit.gpgsign true
```

**Agents must verify SSH signing is active in every worktree before committing:**
```bash
git config --local commit.gpgsign   # must return "true"
```

The private key is held in Bitwarden's SSH agent. Signing uses `ssh-agent` — no key file on disk.

### ⚠️ Verification gotcha

`git log --show-signature` prints "No signature" when `gpg.ssh.allowedSignersFile` is not configured — even though the commit IS signed. This is a local verification issue, not a signing failure. GitHub verifies SSH signatures natively. To check raw signatures:
```bash
git cat-file -p HEAD | grep -A10 "BEGIN SSH SIGNATURE"
```

## Definition of Done (DoD) — Reviewer Gate

**Two-step review before every PR:**

1. **Agent verifies the checklist** — the implementing agent (rust-expert or rust-coder) runs `cargo build / test / clippy / fmt` and checks all DoD items
2. **rust-reviewer verifies the agent's work** — delegate to the reviewer subagent (qwen3.6-plus) for mechanical verification of the DoD checklist
3. **PR created directly** — after reviewer approval, create the PR immediately (no pre-PR user sign-off)

Do NOT proceed to PR creation until the reviewer agent signs off.

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
3. Verify SSH signing is active (git config --local commit.gpgsign)
4. Implement the changes inside the worktree
5. Run cargo build / test / clippy / fmt
6. Request reviewer agent to verify DoD
7. Reviewer approves → create PR from the worktree branch
8. After merge → clean up worktree and branch
```

## Resuming After Interruption

If the agent session crashes, loses power, or is restarted, the next session MUST check for in-progress work before starting anything new. The GitHub issue + worktree branch combo is the sole indicator of active tasks.

### Resume Protocol

When a new session initializes:

**1. Check for open issues assigned to decko:**
```bash
gh issue list --assignee decko --state open
```

**2. For each open issue, check if a worktree branch exists:**
```bash
git branch -a | grep "task/<issue-number>"
```
The branch naming convention `task/<issue-number>-<slug>` makes this a direct lookup.

**3. If a worktree exists → this is an active, in-progress task:**
- Enter the worktree: `cd .worktrees/task/<issue-number>-*`
- `git status` → see uncommitted changes (work in flight)
- `git log --oneline -5` → see what's been committed so far
- Re-read the issue body to re-derive the task scope
- Continue implementation from current state

**4. If no worktree exists → issue is queued but not started:**
- Pick the lowest-numbered open issue in the current milestone
- Create worktree, create branch, begin work

**5. After completing a task, always close the issue.**
An open issue with a matching worktree branch is the system's only indicator of "work in progress." Nothing else is needed — no checkpoint files, no progress comments, no external state.

### Why this works

Each PR/issue in this project is sized at ~100–150 lines. If the agent crashes mid-implementation, at most 150 lines of uncommitted work are lost. The issue description contains the full scope. The worktree branch has whatever was committed. This is a stateless resume — the agent re-derives everything from git and GitHub state.

## Memory

Every session starts with zero conversation history. The resumability protocol recovers **what** (code state, task scope) but not **why** (decisions, rejected alternatives, discovered gotchas). The MEMORY.md file bridges this gap.

### Location

```
~/.local/share/opencode/projects/carv/memory.md
```

Lives outside the repo — survives worktree cleanup, branch deletion, and full repo removal. Not tracked by git.

### When to read

**At session start, before anything else** — including before the resume protocol. This gives the agent immediate context about the project, active work, and key decisions.

### When to write

After any non-trivial decision or discovery:
- Choosing between multiple implementation options
- Discovering a version incompatibility or build gotcha
- Changing a workflow rule or convention
- Completing a milestone (update Active State)

Format each entry as:
```markdown
### YYYY-MM-DD — Brief title
- **What:** [one sentence]
- **Decision:** [what we chose]
- **Why:** [1-2 sentences of rationale]
```

### What it contains

| Section | Purpose |
|---------|---------|
| Active State | Which milestone, which issue, worktree path |
| Key Decisions | Dated entries with rationale |
| Conventions | Rules and patterns the project follows |
| Gotchas | Things that tripped us up, to avoid repeating |
| Completed | Closed issue numbers with short description |

**This is NOT a replacement for issues.** Issues are the task tracker. MEMORY.md is the context bridge between sessions. Decisions made in MEMORY.md should reference their issue numbers.

- `anyhow` for application-level errors (the agent loop, CLI)
- `thiserror` for library-level errors (LLM provider, LSP transport, tree-sitter module)
- Tool errors are returned as `ToolResult` strings to the LLM (it can retry/recover)
- LLM API errors: 3 retries with exponential backoff, respecting `retry-after` headers
- LSP server crashes: one restart attempt, then mark language's LSP tools unavailable

## Code Style

- Rust edition 2021
- `clap` derive for CLI, `serde` derive for wire types
- **Naming:** Project prefix is `Carv` not `Carve` — the crate is `carv`. Use `CarvArgs`, `CarvConfig`, etc.
- `tracing` + `tracing-subscriber` for structured logging (not `println!`)
- Trait methods return `impl Future<Output = Result<T>> + Send` (no boxed futures)
- Stream results via `Pin<Box<dyn Stream<Item = Result<T>> + Send>>`
- Read-only vs. write tool distinction is informational only (shown in verbose/debug output)

## Testing Strategy

- Unit tests: per-tool handler with mock fs/inputs, anchor generation, wire format parsing
- Integration tests: agent loop with mock LLM + mock tools, fixture projects
- LSP tests: real language servers against fixture projects (spawn, sync, crash recovery)

### SCM Query File Review

Tree-sitter query files (`.scm`) are **not Rust** — they are S-expression patterns that match AST node types. Reviewing them requires:
- Checking that `@definition.*` captures match the correct node types for each grammar
- Verifying that `@name.*` captures reference the right child nodes within definitions
- Testing against fixture files to confirm captures fire correctly

These files are a distinct review category from Rust code. Don't review them like logic — review them like templates against a known grammar.

## Dependencies to Know

- `tokio` — multi-threaded runtime, process spawning, channels
- `tree-sitter` + `tree-sitter-rust/python/typescript` — verify compatibility on crates.io. The design doc says "pin to same major version" but grammar crates frequently lag 1-2 versions behind the core crate. Resolve version conflicts before adding these deps (tracked in issue #4).
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
