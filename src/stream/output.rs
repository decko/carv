// Stream output formatters: text, JSON, and stream-JSON (JSONL).
//
// Each formatter implements the [`StreamOutput`] trait and can be created
// via [`create_formatter`] for dynamic dispatch based on CLI flags.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde::Serialize;
use tokio::io::{self, AsyncWrite, AsyncWriteExt};

use crate::cli::OutputFormat;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Token usage statistics emitted in the [`StreamEvent::Done`] event.
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
}

/// Every event that the agent loop can hand to a [`StreamOutput`] formatter.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    /// Plain text content from the LLM.
    #[serde(rename = "text")]
    Text { content: String },
    /// Thinking/reasoning content (from extended thinking).
    #[serde(rename = "thinking")]
    Thinking { content: String },
    /// A tool about to be invoked.
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Result from a tool execution.
    #[serde(rename = "tool_result")]
    ToolResult { id: String, content: String },
    /// End-of-stream marker with usage statistics.
    #[serde(rename = "done")]
    Done { turns: u32, usage: Usage },
}

// ---------------------------------------------------------------------------
// StreamOutput trait
// ---------------------------------------------------------------------------

/// A trait for streaming output formatters.
///
/// Every implementor must be [`Send`] so that it can be held across `.await`
/// points in the agent loop.  Methods return boxed, `Send` futures so the
/// trait remains object-safe (required by [`create_formatter`]).
pub trait StreamOutput: Send {
    /// Deliver a single event to the formatter.
    fn emit(&mut self, event: StreamEvent)
        -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Finalize the output (flush buffers, close JSON structures, etc.).
    fn finish(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// TextFormatter
// ---------------------------------------------------------------------------

/// Plain-text output formatter.
///
/// | Event         | Behaviour                                  |
/// |---------------|--------------------------------------------|
/// | `Text`        | Write content directly (no added newline). |
/// | `Thinking`    | Discarded.                                 |
/// | `ToolUse`     | Write `[tool: {name}]\n` (only if verbose). |
/// | `ToolResult`  | Discarded.                                 |
/// | `Done`        | Write a usage summary line.                |
#[derive(Debug)]
pub struct TextFormatter<W: AsyncWrite + Unpin + Send> {
    writer: W,
    verbose: bool,
}

impl<W: AsyncWrite + Unpin + Send> TextFormatter<W> {
    /// Create a new `TextFormatter` that writes into `writer`.
    pub fn new(writer: W, verbose: bool) -> Self {
        Self { writer, verbose }
    }
}

impl<W: AsyncWrite + Unpin + Send> StreamOutput for TextFormatter<W> {
    fn emit(
        &mut self,
        event: StreamEvent,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            match event {
                StreamEvent::Text { content } => {
                    self.writer.write_all(content.as_bytes()).await?;
                    self.writer.flush().await?;
                }
                StreamEvent::ToolUse { name, .. } => {
                    if self.verbose {
                        let line = format!("[tool: {name}]\n");
                        self.writer.write_all(line.as_bytes()).await?;
                        self.writer.flush().await?;
                    }
                }
                StreamEvent::Done { turns, usage } => {
                    let line = format!(
                        "\nDone. {turns} turns, {} in / {} out / {} cache tokens.\n",
                        usage.input_tokens, usage.output_tokens, usage.cache_read_tokens,
                    );
                    self.writer.write_all(line.as_bytes()).await?;
                    self.writer.flush().await?;
                }
                // Thinking and ToolResult are invisible in text mode.
                StreamEvent::Thinking { .. } | StreamEvent::ToolResult { .. } => {}
            }
            Ok(())
        })
    }

    fn finish(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(self.writer.flush().await?) })
    }
}

// ---------------------------------------------------------------------------
// JsonFormatter
// ---------------------------------------------------------------------------

/// Accumulates all events and writes them as a single JSON array on finish.
#[derive(Debug)]
pub struct JsonFormatter<W: AsyncWrite + Unpin + Send> {
    writer: W,
    events: Vec<StreamEvent>,
}

impl<W: AsyncWrite + Unpin + Send> JsonFormatter<W> {
    /// Create a new `JsonFormatter` that writes into `writer`.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            events: Vec::new(),
        }
    }
}

impl<W: AsyncWrite + Unpin + Send> StreamOutput for JsonFormatter<W> {
    fn emit(
        &mut self,
        event: StreamEvent,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.events.push(event);
            Ok(())
        })
    }

    fn finish(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async {
            let json = serde_json::json!({"events": &self.events});
            let text = serde_json::to_string(&json)?;
            self.writer.write_all(text.as_bytes()).await?;
            self.writer.flush().await?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// StreamJsonFormatter (JSONL)
// ---------------------------------------------------------------------------

/// Writes every event as an individual JSON line (JSONL / newline-delimited JSON).
#[derive(Debug)]
pub struct StreamJsonFormatter<W: AsyncWrite + Unpin + Send> {
    writer: W,
}

impl<W: AsyncWrite + Unpin + Send> StreamJsonFormatter<W> {
    /// Create a new `StreamJsonFormatter` that writes into `writer`.
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: AsyncWrite + Unpin + Send> StreamOutput for StreamJsonFormatter<W> {
    fn emit(
        &mut self,
        event: StreamEvent,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let mut line = serde_json::to_string(&event)?;
            line.push('\n');
            self.writer.write_all(line.as_bytes()).await?;
            self.writer.flush().await?;
            Ok(())
        })
    }

    fn finish(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(self.writer.flush().await?) })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a [`StreamOutput`] formatter matching the requested output format.
///
/// The returned formatter writes to `stdout` and is suitable for use in the
/// agent loop. The `verbose` flag controls whether tool-use lines appear in
/// text mode output.
pub fn create_formatter(format: OutputFormat, verbose: bool) -> Box<dyn StreamOutput> {
    create_formatter_with_writer(format, verbose, io::stdout())
}

/// Create a [`StreamOutput`] formatter writing to the given writer.
///
/// Useful for testing or for directing output to files instead of stdout.
pub fn create_formatter_with_writer<W: AsyncWrite + Unpin + Send + 'static>(
    format: OutputFormat,
    verbose: bool,
    writer: W,
) -> Box<dyn StreamOutput> {
    match format {
        OutputFormat::Text => Box::new(TextFormatter::new(writer, verbose)),
        OutputFormat::Json => Box::new(JsonFormatter::new(writer)),
        OutputFormat::StreamJson => Box::new(StreamJsonFormatter::new(writer)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncWrite;

    /// A simple in-memory buffer that implements [`tokio::io::AsyncWrite`]
    /// so we can capture formatter output without writing to a real terminal.
    #[derive(Debug, Default)]
    struct TestBuf {
        data: Vec<u8>,
    }

    impl TestBuf {
        fn into_string(self) -> String {
            String::from_utf8(self.data).expect("TestBuf contains valid UTF-8")
        }
    }

    impl AsyncWrite for TestBuf {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.data.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    // -----------------------------------------------------------------------
    // TextFormatter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn text_formatter_writes_text_content() {
        let mut fmt = TextFormatter::new(TestBuf::default(), false);
        fmt.emit(StreamEvent::Text {
            content: "Hello, world!".into(),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        assert_eq!(fmt.writer.into_string(), "Hello, world!");
    }

    #[tokio::test]
    async fn text_formatter_shows_tool_use_with_verbose() {
        let mut fmt = TextFormatter::new(TestBuf::default(), true);
        fmt.emit(StreamEvent::ToolUse {
            id: "t1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
        })
        .await
        .unwrap();

        assert_eq!(fmt.writer.into_string(), "[tool: read_file]\n");
    }

    #[tokio::test]
    async fn text_formatter_suppresses_tool_use_without_verbose() {
        let mut fmt = TextFormatter::new(TestBuf::default(), false);
        fmt.emit(StreamEvent::ToolUse {
            id: "t1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        assert!(
            fmt.writer.data.is_empty(),
            "tool use should be invisible without verbose"
        );
    }

    #[tokio::test]
    async fn text_formatter_discards_thinking() {
        let mut fmt = TextFormatter::new(TestBuf::default(), false);
        fmt.emit(StreamEvent::Thinking {
            content: "Let me think...".into(),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        assert!(fmt.writer.data.is_empty());
    }

    #[tokio::test]
    async fn text_formatter_discards_tool_result() {
        let mut fmt = TextFormatter::new(TestBuf::default(), false);
        fmt.emit(StreamEvent::ToolResult {
            id: "t1".into(),
            content: "some result".into(),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        assert!(fmt.writer.data.is_empty());
    }

    #[tokio::test]
    async fn text_formatter_emits_done_summary() {
        let usage = Usage {
            input_tokens: 1200,
            output_tokens: 380,
            cache_read_tokens: 890,
        };
        let mut fmt = TextFormatter::new(TestBuf::default(), false);
        fmt.emit(StreamEvent::Done {
            turns: 5,
            usage: usage.clone(),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        let out = fmt.writer.into_string();
        assert!(
            out.contains("Done. 5 turns"),
            "expected done summary, got: {out:?}"
        );
        assert!(out.contains("1200 in"));
        assert!(out.contains("380 out"));
        assert!(out.contains("890 cache tokens"));
    }

    #[tokio::test]
    async fn text_formatter_interleaves_text_and_tool_use() {
        let mut fmt = TextFormatter::new(TestBuf::default(), true);
        fmt.emit(StreamEvent::Text {
            content: "Let me check.".into(),
        })
        .await
        .unwrap();
        fmt.emit(StreamEvent::ToolUse {
            id: "t1".into(),
            name: "read_file".into(),
            input: serde_json::Value::Null,
        })
        .await
        .unwrap();
        fmt.emit(StreamEvent::Text {
            content: " Found it.".into(),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        let out = fmt.writer.into_string();
        assert_eq!(out, "Let me check.[tool: read_file]\n Found it.");
    }

    // -----------------------------------------------------------------------
    // JsonFormatter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn json_formatter_zero_events_produces_empty_array() {
        let mut fmt = JsonFormatter::new(TestBuf::default());
        fmt.finish().await.unwrap();

        let raw = fmt.writer.into_string();
        assert_eq!(raw, r#"{"events":[]}"#);
    }

    #[tokio::test]
    async fn json_formatter_accumulates_events() {
        let mut fmt = JsonFormatter::new(TestBuf::default());
        fmt.emit(StreamEvent::Text {
            content: "Hello".into(),
        })
        .await
        .unwrap();
        fmt.emit(StreamEvent::Thinking {
            content: "hmm".into(),
        })
        .await
        .unwrap();
        assert_eq!(fmt.events.len(), 2);
    }

    #[tokio::test]
    async fn json_formatter_finish_produces_valid_json_object() {
        let mut fmt = JsonFormatter::new(TestBuf::default());
        fmt.emit(StreamEvent::Text {
            content: "Hello".into(),
        })
        .await
        .unwrap();
        fmt.emit(StreamEvent::ToolUse {
            id: "t1".into(),
            name: "grep".into(),
            input: serde_json::json!({"pattern": "foo"}),
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        let raw = fmt.writer.into_string();
        // Should be a JSON object with an "events" array.
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.is_object(), "expected JSON object, got: {raw:?}");
        let events = parsed["events"]
            .as_array()
            .expect("'events' should be an array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "text");
        assert_eq!(events[1]["type"], "tool_use");
    }

    // -----------------------------------------------------------------------
    // StreamJsonFormatter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stream_json_formatter_produces_jsonl() {
        let mut fmt = StreamJsonFormatter::new(TestBuf::default());
        fmt.emit(StreamEvent::Text {
            content: "line1".into(),
        })
        .await
        .unwrap();
        fmt.emit(StreamEvent::ToolUse {
            id: "t1".into(),
            name: "edit".into(),
            input: serde_json::Value::Null,
        })
        .await
        .unwrap();
        fmt.finish().await.unwrap();

        let raw = fmt.writer.into_string();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 JSONL lines, got: {raw:?}");

        // Each line should be valid JSON.
        for line in &lines {
            let val: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("invalid JSONL line {line:?}: {e}"));
            assert!(val.is_object());
        }

        // Verify event types in order.
        let v0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let v1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v0["type"], "text");
        assert_eq!(v1["type"], "tool_use");
    }

    // -----------------------------------------------------------------------
    // Serialisation format tests
    // -----------------------------------------------------------------------

    #[test]
    fn stream_event_serde_tagged_done() {
        let event = StreamEvent::Done {
            turns: 2,
            usage: Usage {
                input_tokens: 1240,
                output_tokens: 380,
                cache_read_tokens: 890,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "done");
        assert_eq!(json["turns"], 2);
        assert_eq!(json["usage"]["input_tokens"], 1240);
        assert_eq!(json["usage"]["output_tokens"], 380);
        assert_eq!(json["usage"]["cache_read_tokens"], 890);
    }

    #[test]
    fn stream_event_serde_tagged_tool_use() {
        let event = StreamEvent::ToolUse {
            id: "t1".into(),
            name: "lsp_rename".into(),
            input: serde_json::json!({"symbol": "getData", "new_name": "fetch_data"}),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "t1");
        assert_eq!(json["name"], "lsp_rename");
        assert_eq!(json["input"]["symbol"], "getData");
    }
}
