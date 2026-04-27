// CLI argument parsing and configuration loading.

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fmt;

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

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::OpenAI => write!(f, "openai"),
        }
    }
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

/// Resolved configuration combining CLI args with environment variables.
pub struct CarvConfig {
    /// Task prompt.
    pub prompt: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Resolved LLM provider.
    pub provider: Provider,
    /// Non-interactive output mode.
    pub print: bool,
    /// Maximum tool-use rounds.
    pub max_turns: u32,
    /// Output format.
    pub output_format: OutputFormat,
    /// Custom system prompt.
    pub system_prompt: Option<String>,
    /// Disallowed tools list.
    pub disallowed_tools: Vec<String>,
    /// Verbose debug output.
    pub verbose: bool,
    /// API key from environment.
    pub api_key: String,
}

impl CarvConfig {
    /// Resolve provider, read API key from env, and populate remaining fields.
    pub fn from_args_and_env(args: CarvArgs) -> Result<Self> {
        let provider = match args.provider {
            Some(p) => p,
            None => {
                let model = args.model.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("No model specified. Use -m <MODEL> or --provider <PROVIDER>.")
                })?;
                auto_detect_provider(model)?
            }
        };

        let api_key = match provider {
            Provider::Anthropic => std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?,
            Provider::OpenAI => std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?,
        };

        Ok(CarvConfig {
            prompt: args.prompt,
            model: args.model,
            provider,
            print: args.print,
            max_turns: args.max_turns,
            output_format: args.output_format,
            system_prompt: args.system_prompt,
            disallowed_tools: args.disallowed_tools,
            verbose: args.verbose,
            api_key,
        })
    }
}

impl fmt::Debug for CarvConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CarvConfig")
            .field("prompt", &self.prompt)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("print", &self.print)
            .field("max_turns", &self.max_turns)
            .field("output_format", &self.output_format)
            .field("system_prompt", &self.system_prompt)
            .field("disallowed_tools", &self.disallowed_tools)
            .field("verbose", &self.verbose)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Auto-detect LLM provider from model name prefix.
fn auto_detect_provider(model: &str) -> Result<Provider> {
    if model.starts_with("claude-") || model.starts_with("anthropic/") {
        Ok(Provider::Anthropic)
    } else if model.starts_with("gpt-")
        || model.starts_with("chatgpt-")
        || model.starts_with("o1-")
        || model.starts_with("o3-")
        || model.starts_with("o4-")
    {
        Ok(Provider::OpenAI)
    } else {
        bail!(
            "Cannot auto-detect provider for model '{}' — use --provider",
            model
        );
    }
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

    // --- CarvConfig tests ---
    //
    // Note: Tests that manipulate ANTHROPIC_API_KEY are consolidated into a single
    // test to avoid race conditions from parallel env var access (all share the same
    // global state). OPENAI_API_KEY tests can be separate because they only set
    // (never remove) that var, so they don't race.

    #[test]
    fn openai_auto_detect_gpt() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "gpt-4o"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::OpenAI);
        assert_eq!(config.api_key, "test-key");
    }

    #[test]
    fn openai_auto_detect_o3() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "o3-mini"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::OpenAI);
    }

    #[test]
    fn openai_auto_detect_chatgpt() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "chatgpt-4o-latest"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::OpenAI);
    }

    #[test]
    fn openai_auto_detect_o4() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "o4-mini"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::OpenAI);
    }

    #[test]
    fn unknown_model_without_provider_errors() {
        let args = CarvArgs::try_parse_from(["carv", "-m", "unknown-model"]).unwrap();
        let result = CarvConfig::from_args_and_env(args);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Cannot auto-detect provider"), "error: {err}");
    }

    #[test]
    fn no_model_specified_errors() {
        let args = CarvArgs::try_parse_from(["carv"]).unwrap();
        let result = CarvConfig::from_args_and_env(args);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No model specified"), "error: {err}");
    }

    /// Consolidated test for all ANTHROPIC_API_KEY-dependent scenarios.
    /// Runs sequentially (within one test) to avoid races with other tests
    /// that set/remove the same env var.
    #[test]
    fn anthropic_env_var_scenarios() {
        // 1. Missing key → error
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "claude-sonnet-4-20250514"]).unwrap();
        let result = CarvConfig::from_args_and_env(args);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("ANTHROPIC_API_KEY"), "error: {err}");

        // 2. Auto-detect claude- prefix → Anthropic
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };
        let args = CarvArgs::try_parse_from(["carv", "-m", "claude-sonnet-4-20250514"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::Anthropic);
        assert_eq!(config.api_key, "test-key");

        // 2b. Auto-detect anthropic/ prefix → Anthropic
        let args = CarvArgs::try_parse_from(["carv", "-m", "anthropic/claude-sonnet"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::Anthropic);

        // 3. Explicit --provider overrides model-based auto-detect
        //    model=gpt-4o suggests OpenAI, but --provider anthropic overrides
        let args =
            CarvArgs::try_parse_from(["carv", "-m", "gpt-4o", "--provider", "anthropic"]).unwrap();
        let config = CarvConfig::from_args_and_env(args).unwrap();
        assert_eq!(config.provider, Provider::Anthropic);
        assert_eq!(config.api_key, "test-key");
    }
}
