// Word-based stable anchors for hash-referenced line editing.
//
// Every line of code can be referenced by a deterministic, human-readable
// word (an "anchor"). Anchors are derived from a 64-bit FNV-1a hash of
// line content, mapped into a fixed dictionary of ~500 common English
// words. They remain stable across insertions and deletions elsewhere in
// the file — unlike line numbers, which shift.
//
// Duplicate-line disambiguation (occurrence indices like `Delta.1`)
// is handled by [`super::state`] — this module only provides the base
// word mapping.

use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Word dictionary — ~500 capitalized English words, 4-8 chars.
// ---------------------------------------------------------------------------

/// Lazy-initialized vector of words from the word list.
///
/// Words are loaded from `words.txt` at compile time via [`include_str!`]
/// and split into a [`Vec`] at first use via [`LazyLock`].
static WORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let data = include_str!("words.txt");
    data.split_whitespace().collect()
});

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return the total number of words in the dictionary.
pub fn dictionary_size() -> usize {
    WORDS.len()
}

/// Hash `line` content and return a deterministic word from the dictionary.
///
/// Leading and trailing whitespace is stripped before hashing, so
/// `"  let x = 1;"` and `"let x = 1;"` produce the same anchor.
/// The same line content **always** maps to the same word, regardless
/// of where the line appears in a file or how many times the function
/// is called. This property is critical: anchors must be stable so the
/// LLM can reference lines consistently across tool calls.
///
/// # Determinism
///
/// The hash is FNV-1a 64-bit — a deterministic, non-cryptographic hash
/// that produces the same output for the same input across all platforms
/// and process invocations. Unlike `std::collections::hash_map::DefaultHasher`
/// (which uses a random SipHash key per process), FNV-1a's output depends
/// only on the input bytes.
pub fn word_for_line(line: &str) -> &'static str {
    let trimmed = line.trim();
    let hash = fnv64(trimmed.as_bytes());
    let idx = (hash as usize) % WORDS.len();
    WORDS[idx]
}

// ---------------------------------------------------------------------------
// FNV-1a 64-bit hash
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit hash.
///
/// Fowler–Noll–Vo hash with the alternate (1a) variant: XOR a byte, then
/// multiply by the FNV prime. Deterministic, fast, and has good avalanche
/// properties for small inputs like individual lines of code.
///
/// Constants are per the FNV spec:
/// - offset basis: 0xcbf29ce484222325
/// - prime:        0x100000001b3
fn fnv64(data: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn dictionary_has_enough_words() {
        let n = dictionary_size();
        assert!(n >= 400, "expected at least 400 words, got {n}",);
    }

    #[test]
    fn dictionary_has_no_duplicates() {
        let mut seen = HashSet::new();
        for word in WORDS.iter() {
            assert!(seen.insert(word), "duplicate word in dictionary: {word}",);
        }
    }

    #[test]
    fn same_line_produces_same_anchor() {
        let line = "pub fn calculate(x: i32) -> i32 {";
        let a = word_for_line(line);
        let b = word_for_line(line);
        assert_eq!(a, b, "same line must yield identical anchor");
    }

    #[test]
    fn anchors_are_stable_across_invocations() {
        // FNV-1a is deterministic — calling word_for_line repeatedly
        // with the same input must yield the same result every time.
        let expected = word_for_line("let x = 42;");
        for _ in 0..1000 {
            assert_eq!(
                word_for_line("let x = 42;"),
                expected,
                "anchor must be stable across repeated calls",
            );
        }
    }

    #[test]
    fn anchors_are_stable_across_func_boundaries() {
        // Different calling contexts must not affect the hash.
        let a = word_for_line("fn foo() {");
        let b = {
            let line = "fn foo() {".to_string();
            word_for_line(&line)
        };
        assert_eq!(a, b);
    }

    #[test]
    fn whitespace_is_trimmed_before_hashing() {
        let a = word_for_line("  let x = 1;  ");
        let b = word_for_line("let x = 1;");
        assert_eq!(a, b, "leading/trailing whitespace must not affect anchor",);
    }

    #[test]
    fn empty_line_is_consistent() {
        let a = word_for_line("");
        let b = word_for_line("   ");
        let c = word_for_line("\t");
        assert_eq!(a, b, "empty and whitespace-only lines must match");
        assert_eq!(a, c, "tab-only line must match empty line");
    }

    #[test]
    fn collision_rate_on_code_lines_is_acceptable() {
        // Measure collision rate among DISTINCT code lines only.
        // Duplicate lines (e.g., multiple "}" or blank lines) always
        // produce the same anchor — that's by design, and the
        // occurrence index disambiguates them. We only care that
        // genuinely different lines rarely map to the same word.

        let lines = &[
            // Distinct Rust lines
            "fn main() {",
            "    let x = 42;",
            "    println!(\"Hello\");",
            "pub struct Config {",
            "    pub verbose: bool,",
            "    pub max_turns: u32,",
            "impl Config {",
            "    pub fn new() -> Self {",
            "        Self { verbose: false, max_turns: 50 }",
            "async fn process() -> anyhow::Result<()> {",
            "    let data = reqwest::get(url).await?;",
            "    let text = data.text().await?;",
            "    Ok(())",
            "use std::collections::HashMap;",
            "#[derive(Debug, Clone, Serialize)]",
            "pub enum OutputFormat {",
            "    Text,",
            "    Json,",
            "    StreamJson,",
            "if x > 0 {",
            "    return Ok(());",
            "} else {",
            "    bail!(\"expected positive\");",
            "match event {",
            "    Event::Text(c) => write!(out, \"{c}\")?,",
            "    Event::Done { usage } => break,",
            "    _ => continue,",
            "for item in items.iter() {",
            "    total += item.value;",
            "while let Some(line) = rx.recv().await {",
            "    buffer.push_str(&line);",
            "let hash = hasher.finalize_fixed();",
            "self.tokens_used += delta;",
            "}",
            "  ",
            "x",
            "return;",
            "Ok(())?;",
            "// TODO: fix this",
            "/* multi",
            "line",
            "comment */",
            // Distinct TypeScript lines
            "import { foo } from './bar';",
            "export default class App extends React.Component {",
            "    render() {",
            "        return <div>Hello</div>;",
            "const x: number = 42;",
            "interface User {",
            "    name: string;",
            "    age: number;",
            "type Result<T> = Ok<T> | Err;",
            // Distinct Python lines
            "def process(data):",
            "    return [x * 2 for x in data if x > 0]",
            "finally:",
            // Distinct closing patterns
            "        }",
            "    }",
        ];

        // Collision rate: how many DISTINCT lines produce the same anchor.
        let anchors: Vec<&str> = lines.iter().map(|l| word_for_line(l)).collect();
        let unique: HashSet<_> = anchors.iter().copied().collect();
        let uniqueness = unique.len() as f64 / anchors.len() as f64;

        // With a ~1500-word dictionary and ~60 distinct code lines,
        // we expect very few collisions. A uniqueness >= 90% is easily
        // achievable if the hash distributes well.
        let expected_threshold = 0.80;
        assert!(
            uniqueness >= expected_threshold,
            "collision rate too high: {:.1}% unique (threshold {:.1}%), \
             dict size = {}",
            uniqueness * 100.0,
            expected_threshold * 100.0,
            dictionary_size(),
        );
    }

    #[test]
    fn identical_lines_map_to_same_word() {
        // The design requires that blank lines, single-brace lines, etc.
        // all produce the SAME anchor — the occurrence index (`.1`, `.2`)
        // disambiguates them in AnchorState.
        let blank_a = word_for_line("");
        let blank_b = word_for_line("");
        assert_eq!(blank_a, blank_b);

        let brace = word_for_line("}");
        assert_eq!(brace, word_for_line("}"));

        let close_paren = word_for_line("    }");
        let close_paren2 = word_for_line("    }");
        assert_eq!(close_paren, close_paren2);
    }

    #[test]
    fn word_for_line_returns_valid_dictionary_word() {
        let dict: HashSet<&&str> = WORDS.iter().collect();
        for line in &[
            "fn test() {}",
            "",
            "     ",
            "\t\t",
            "use tokio::runtime::Runtime;",
            "x + y * z / 2",
        ] {
            let word = word_for_line(line);
            assert!(
                dict.contains(&word),
                "word_for_line returned '{word}' which is not in dictionary",
            );
        }
    }
}
