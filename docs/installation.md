# Installation Guide

## System Requirements

- **Rust toolchain** — carv requires Rust 1.75+ (native async fn in traits). The minimum
  tested version is **1.80**. Install via [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **git** — used for project discovery and some tool internals
- **C compiler** — needed only when building from source with tree-sitter grammars
  (gcc, clang, or MSVC). Most systems already have one.

## Install via Cargo

If you have Rust installed, the quickest way is:

```bash
cargo install carv
```

This downloads the source from [crates.io](https://crates.io/crates/carv), compiles
it (with optimisations), and places the binary in `~/.cargo/bin/`. Make sure that
directory is on your `PATH`.

## Build from Source

Clone the repository and build with optimisations:

```bash
git clone https://github.com/decko/carv.git
cd carv
cargo build --release
```

The binary is placed at `target/release/carv`. Add it to your `PATH`, or copy it
to a directory already on your `PATH`:

```bash
cp target/release/carv ~/.cargo/bin/
# or
cp target/release/carv /usr/local/bin/
```

### Build Dependencies

Tree-sitter grammars (Rust, Python, TypeScript) are compiled at build time. A C
compiler is required. On most Linux distributions this is already installed. On
macOS, install Xcode Command Line Tools:

```bash
xcode-select --install
```

On Windows, install [Build Tools for Visual Studio](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
and ensure `cl.exe` is on your `PATH`.

## API Key Setup

carv reads API keys from environment variables only. Never pass keys on the
command line — secrets in arguments leak to shell history and `/proc`.

### Anthropic (Claude)

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

For convenience, add the line to your `~/.bashrc`, `~/.zshrc`, or equivalent
shell configuration file.

### OpenAI (GPT)

```bash
export OPENAI_API_KEY="sk-..."
```

You need at least one key. carv auto-detects the provider from the model name,
or you can set it explicitly with `--provider`.

### Verifying Your Key

A quick smoke test (requires network access):

```bash
carv -p -m claude-sonnet-4-20250514 "say hello"
```

If the key is valid and the API is reachable, you will see a brief response.

## Optional: Language Server Installations

LSP integration (scope-aware rename, cross-file reference finding, diagnostics)
is **planned but not yet implemented**. When it ships, carv will expect the
appropriate language server for your project:

| Language   | Server                   | Install                                                         |
|------------|--------------------------|-----------------------------------------------------------------|
| Rust       | `rust-analyzer`          | `rustup component add rust-analyzer`                            |
| Python     | `ty` (based on Pyright)  | `npm install -g pyright` (planned — LSP not yet implemented)    |
| TypeScript | `typescript-language-server` | `npm install -g typescript-language-server`                |

These servers are spawned lazily (first LSP tool use for that language). You do
not need them for tree-sitter-based operations (read, search, edit, execute).

## Verify Installation

Run the help command to confirm the binary is installed correctly:

```bash
carv --help
```

Expected output:

```
A minimal Rust coding agent with tree-sitter + LSP

Usage: carv [OPTIONS] [PROMPT]

Arguments:
  [PROMPT]  Task prompt. Reads from stdin if not provided and stdin is piped.

Options:
  -m, --model <MODEL>                  Model name. Provider is auto-detected from model name.
      --provider <PROVIDER>            Explicit provider override: anthropic | openai
  -p, --print                          Non-interactive output mode (print result and exit).
      --max-turns <MAX_TURNS>          Maximum number of tool-use rounds [default: 50]
      --output-format <OUTPUT_FORMAT>  Output format: text, json, or stream-json [default: text]
      --system-prompt <SYSTEM_PROMPT>  Custom system prompt to replace the default.
      --disallowed-tools <DISALLOWED_TOOLS>
                                       Comma-separated list of tool names to disable.
  -v, --verbose                        Enable verbose debug output to stderr.
  -h, --help                           Print help
  -V, --version                        Print version
```

## Platform Notes

### Linux

Fully supported. carv uses tokio's multi-threaded runtime and standard async I/O.
Tested on x86_64 and aarch64.

### macOS

Fully supported. Tested on Apple Silicon (M-series) and Intel. If you see a
compilation error about `openssl-sys`, install OpenSSL via Homebrew:

```bash
brew install openssl
export LIBRARY_PATH="$LIBRARY_PATH:/opt/homebrew/lib"  # Apple Silicon
export CPATH="$CPATH:/opt/homebrew/include"             # Apple Silicon
```

### Windows

carv is primarily developed on Linux/macOS. Windows support should work via
MSVC but is less tested. Known caveats:

- **Shell execution:** The `execute_command` tool runs commands via
  `tokio::process::Command`, not through `cmd.exe` with string interpolation.
  Piped stdin and redirects may not work as expected inside commands.
- **Path separators:** carv normalizes paths internally, but language servers
  and tree-sitter grammars may need extra configuration on Windows.
- **C compiler:** Install Build Tools for Visual Studio 2022 for tree-sitter
  grammar compilation.
- **Line endings:** carv handles CRLF transparently. Hash anchors are computed
  from logical line content, so CRLF vs. LF does not affect anchor stability.

## Troubleshooting

### "No model specified" error

carv requires a model name when auto-detecting the provider. Provide one:

```bash
carv -m claude-sonnet-4-20250514 "explain this file"
```

Or set the provider explicitly:

```bash
carv --provider anthropic "explain this file"
```

### "Cannot auto-detect provider" error

The model name does not match any known prefix. Use `--provider` explicitly:

```bash
carv -m my-custom-model --provider openai "task"
```

Known auto-detect prefixes:
- `claude-*`, `anthropic/*` → Anthropic
- `gpt-*`, `chatgpt-*`, `o1-*`, `o3-*`, `o4-*` → OpenAI

### "ANTHROPIC_API_KEY environment variable not set"

The `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` environment variable is missing.
Set it before running carv:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
carv -m claude-sonnet-4-20250514 "task"
```

### Build fails with a C compiler error

Tree-sitter grammars need a C compiler. Install one:

- **Debian/Ubuntu:** `sudo apt install build-essential`
- **Fedora:** `sudo dnf install gcc`
- **macOS:** `xcode-select --install`
- **Windows:** Install Build Tools for Visual Studio 2022

### "command not found: carv"

After `cargo install`, ensure `~/.cargo/bin` is on your `PATH`:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### cargo install fails with dependency resolution errors

Try building from source instead. This uses the exact dependency versions
specified in the repository's `Cargo.lock`:

```bash
git clone https://github.com/decko/carv.git
cd carv
cargo build --release
```
