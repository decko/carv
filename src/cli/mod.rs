// CLI argument parsing and configuration loading.

use clap::Parser;

/// Main CLI configuration for carv.
#[derive(Parser, Debug)]
#[command(
    name = "carv",
    version,
    about = "Minimal Rust Coding Agent with Tree-sitter + LSP"
)]
pub struct CarveArgs {
    /// Task prompt. Reads from stdin if not provided and stdin is piped.
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Model name. Provider is auto-detected from model name.
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Explicit provider override: anthropic | openai.
    #[arg(long = "provider")]
    pub provider: Option<Provider>,

    /// Non-interactive output mode (print result and exit).
    #[arg(short = 'p', long = "print")]
    pub print: bool,

    /// Maximum number of tool-use rounds (default: 50).
    #[arg(long = "max-turns", default_value_t = 50)]
    pub max_turns: u32,

    /// Output format: text, json, or stream-json (default: text).
    #[arg(long = "output-format", default_value = "text")]
    pub output_format: OutputFormat,

    /// Custom system prompt to replace the default.
    #[arg(long = "system-prompt")]
    pub system_prompt: Option<String>,

    /// Comma-separated list of tool names to disable.
    #[arg(long = "disallowed-tools", value_delimiter = ',')]
    pub disallowed_tools: Vec<String>,

    /// Enable verbose debug output to stderr.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

/// Supported LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Provider {
    /// Anthropic (Claude) API.
    Anthropic,
    /// OpenAI (GPT) API.
    #[value(name = "openai")]
    OpenAI,
}

/// Output format for streaming results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Plain text output.
    Text,
    /// Single JSON object after completion.
    Json,
    /// JSON lines stream.
    StreamJson,
}
