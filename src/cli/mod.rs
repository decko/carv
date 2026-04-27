// CLI argument parsing and configuration loading.

use clap::Parser;

/// Main CLI configuration for carv.
#[derive(Parser, Debug)]
#[command(
    name = "carv",
    version,
    about = "Minimal Rust Coding Agent with Tree-sitter + LSP"
)]
pub struct CarvArgs {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_prompt_as_positional_arg() {
        let args = CarvArgs::try_parse_from(["carv", "hello world"]).unwrap();
        assert_eq!(args.prompt.as_deref(), Some("hello world"));
    }

    #[test]
    fn prompt_is_none_when_omitted() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        assert_eq!(args.prompt, None);
    }

    #[test]
    fn disallowed_tools_splits_comma_separated() {
        let args = CarvArgs::try_parse_from(["carv", "--disallowed-tools", "a,b,c"]).unwrap();
        assert_eq!(args.disallowed_tools, vec!["a", "b", "c"]);
    }

    #[test]
    fn disallowed_tools_empty_when_not_provided() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        assert!(args.disallowed_tools.is_empty());
    }

    #[test]
    fn provider_enum_round_trip() {
        let args = CarvArgs::try_parse_from(["carv", "--provider", "anthropic"]).unwrap();
        assert_eq!(args.provider, Some(Provider::Anthropic));

        let args = CarvArgs::try_parse_from(["carv", "--provider", "openai"]).unwrap();
        assert_eq!(args.provider, Some(Provider::OpenAI));
    }

    #[test]
    fn output_format_enum_round_trip() {
        let args = CarvArgs::try_parse_from(["carv", "--output-format", "json"]).unwrap();
        assert_eq!(args.output_format, OutputFormat::Json);

        let args = CarvArgs::try_parse_from(["carv", "--output-format", "stream-json"]).unwrap();
        assert_eq!(args.output_format, OutputFormat::StreamJson);
    }

    #[test]
    fn max_turns_default_is_50() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        assert_eq!(args.max_turns, 50);
    }

    #[test]
    fn max_turns_custom_value() {
        let args = CarvArgs::try_parse_from(["carv", "--max-turns", "10"]).unwrap();
        assert_eq!(args.max_turns, 10);
    }

    #[test]
    fn verbose_flag_is_false_by_default() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        assert!(!args.verbose);
    }

    #[test]
    fn verbose_flag_enabled() {
        let args = CarvArgs::try_parse_from(["carv", "-v"]).unwrap();
        assert!(args.verbose);
    }

    #[test]
    fn print_flag_is_false_by_default() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        assert!(!args.print);
    }

    #[test]
    fn model_option() {
        let args = CarvArgs::try_parse_from(["carv", "-m", "claude-sonnet-4-20250514"]).unwrap();
        assert_eq!(args.model.as_deref(), Some("claude-sonnet-4-20250514"));
    }
}
