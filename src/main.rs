use carv::agent::agent_loop::run_agent_loop;
use carv::agent::context::{build_system_prompt, detect_git_branch};
use carv::cli::{CarvArgs, CarvConfig, OutputFormat};
use carv::hashing::state::AnchorState;
use carv::llm::anthropic::AnthropicProvider;
use carv::llm::openai::OpenAIProvider;
use carv::llm::types::RequestConfig;
use carv::stream::output::{create_formatter, StreamOutput};
use carv::tools::{default_tools, ToolContext, ToolRegistry};
use carv::treesitter::ParserCache;
use clap::Parser;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let args = CarvArgs::parse();
    let config = CarvConfig::from_args_and_env(args)?;

    // Initialize tracing (stderr, for debug/verbose output)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(if config.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    // Get prompt from args or stdin
    let user_prompt = match &config.prompt {
        Some(p) => p.clone(),
        None => read_stdin()?,
    };

    // Workspace root (current directory)
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Detect git branch
    let git_branch = detect_git_branch(&workspace_root);

    // Build tool context (shared state for all tools)
    let tool_ctx = ToolContext {
        workspace_root: workspace_root.clone(),
        anchor_state: Arc::new(Mutex::new(AnchorState::new())),
        parser_cache: Arc::new(Mutex::new(ParserCache::new())),
    };

    // Build tool registry with default tools, filtering disallowed ones
    let registry = ToolRegistry::new(default_tools(), config.disallowed_tools.clone());

    // Build system prompt (uses filtered tool definitions)
    let system_prompt_msg = build_system_prompt(
        config.system_prompt.as_deref(),
        &registry.tool_defs(),
        &workspace_root,
        &git_branch,
    );

    // Extract text from the system message
    let system_prompt_text = match system_prompt_msg.content.first() {
        Some(block) => match &block.content {
            carv::llm::types::ContentType::Text { text } => text.clone(),
            _ => {
                tracing::warn!("system prompt has unexpected content type, using fallback");
                "You are a coding agent.".to_string()
            }
        },
        None => {
            tracing::warn!("system prompt has empty content, using fallback");
            "You are a coding agent.".to_string()
        }
    };

    // Create LLM provider
    let model = config.model.as_deref().unwrap_or(match config.provider {
        carv::cli::Provider::Anthropic => "claude-sonnet-4-20250514",
        carv::cli::Provider::OpenAI => "gpt-4o",
    });
    let provider: Box<dyn carv::llm::provider::LlmProvider> = match config.provider {
        carv::cli::Provider::Anthropic => Box::new(AnthropicProvider::new(
            config.api_key.clone(),
            model.to_string(),
        )),
        carv::cli::Provider::OpenAI => Box::new(OpenAIProvider::new(
            config.api_key.clone(),
            model.to_string(),
        )),
    };

    // Determine output format and verbosity for the formatter.
    // --print forces text-only output without tool-use logging.
    let output_format = if config.print {
        OutputFormat::Text
    } else {
        config.output_format
    };
    let formatter_verbose = if config.print { false } else { config.verbose };
    let mut output: Box<dyn StreamOutput> = create_formatter(output_format, formatter_verbose);

    // Build request config (extended thinking enabled for Claude Sonnet)
    let request_config = RequestConfig {
        max_tokens: 8192,
        temperature: None,
        top_p: None,
        stop_sequences: vec![],
        thinking: true,
        thinking_budget: Some(1024),
    };

    // Run agent loop
    let summary = run_agent_loop(
        provider.as_ref(),
        &registry,
        &tool_ctx,
        &system_prompt_text,
        &user_prompt,
        &request_config,
        config.max_turns,
        200_000, // 200k context window (Claude Sonnet)
        output.as_mut(),
    )
    .await?;

    // Print final summary to stderr (tracing)
    tracing::info!(
        turns = summary.turns,
        input_tokens = summary.total_input_tokens,
        output_tokens = summary.total_output_tokens,
        estimated_cost = format!("${:.4}", summary.estimated_cost_usd),
        "agent run complete"
    );

    Ok(())
}

/// Read stdin if available (piped input). Returns an error if no prompt data is provided.
fn read_stdin() -> anyhow::Result<String> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        anyhow::bail!("No prompt provided. Pass a prompt as an argument or pipe one via stdin.");
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    if input.trim().is_empty() {
        anyhow::bail!("No prompt provided. Stdin was empty.");
    }
    Ok(input)
}
