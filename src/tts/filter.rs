use std::sync::OnceLock;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;

pub fn filter_for_tts(input: &str) -> String {
    let parsed_cleaned = clean_tts_candidate(&render_markdown_for_tts(input));
    let raw_cleaned = clean_tts_candidate(&strip_raw_fallback_noise(input));

    if should_prefer_raw_markdown_text(&parsed_cleaned, &raw_cleaned) {
        return raw_cleaned;
    }

    parsed_cleaned
}

fn clean_tts_candidate(input: &str) -> String {
    normalize_tts_text(&strip_special_character_noise(
        &strip_streaming_markdown_fragments(input),
    ))
}

fn render_markdown_for_tts(input: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(input, options);
    let mut output = String::with_capacity(input.len());
    let mut skip_code_block_depth = 0usize;

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                skip_code_block_depth += 1;
            }
            Event::End(TagEnd::CodeBlock) => {
                skip_code_block_depth = skip_code_block_depth.saturating_sub(1);
            }
            Event::Text(text) | Event::Code(text) if skip_code_block_depth == 0 => {
                output.push_str(&text);
            }
            Event::InlineMath(math) if skip_code_block_depth == 0 => {
                output.push('$');
                output.push_str(&math);
                output.push('$');
            }
            Event::DisplayMath(math) if skip_code_block_depth == 0 => {
                output.push_str("\n$$");
                output.push_str(&math);
                output.push_str("$$\n");
            }
            Event::SoftBreak | Event::HardBreak if skip_code_block_depth == 0 => {
                output.push('\n');
            }
            Event::Rule if skip_code_block_depth == 0 => {
                output.push_str("\n\n");
            }
            Event::FootnoteReference(label) if skip_code_block_depth == 0 => {
                output.push_str(&label);
            }
            Event::TaskListMarker(checked) if skip_code_block_depth == 0 => {
                output.push_str(if checked { "done " } else { "" });
            }
            Event::Html(html) | Event::InlineHtml(html) if skip_code_block_depth == 0 => {
                output.push_str(&html_tag_regex().replace_all(&html, " "));
            }
            Event::Start(Tag::Paragraph)
            | Event::Start(Tag::Heading { .. })
            | Event::Start(Tag::BlockQuote(_))
            | Event::Start(Tag::List(_))
            | Event::Start(Tag::Item)
            | Event::Start(Tag::Link { .. })
            | Event::Start(Tag::Image { .. })
            | Event::Start(Tag::Emphasis)
            | Event::Start(Tag::Strong)
            | Event::Start(Tag::Strikethrough)
            | Event::Start(Tag::Table(_))
            | Event::Start(Tag::TableHead)
            | Event::Start(Tag::TableRow)
            | Event::Start(Tag::TableCell)
            | Event::End(TagEnd::Link)
            | Event::End(TagEnd::Image)
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Table) => {}
            Event::End(TagEnd::Paragraph) if skip_code_block_depth == 0 => {
                output.push_str("\n\n");
            }
            Event::End(TagEnd::Heading(_)) if skip_code_block_depth == 0 => {
                output.push_str(".\n\n");
            }
            Event::End(TagEnd::BlockQuote(_)) | Event::End(TagEnd::List(_))
                if skip_code_block_depth == 0 =>
            {
                output.push('\n');
            }
            Event::End(TagEnd::Item) if skip_code_block_depth == 0 => {
                output.push('\n');
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow)
                if skip_code_block_depth == 0 =>
            {
                output.push('\n');
            }
            Event::End(TagEnd::TableCell) if skip_code_block_depth == 0 => {
                output.push_str("  ");
            }
            _ => {}
        }
    }

    output
}

fn should_prefer_raw_markdown_text(parsed: &str, raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }

    let parsed_score = speech_content_score(parsed);
    let raw_score = speech_content_score(raw);

    if raw_score == 0 {
        return false;
    }

    if parsed_score == 0 {
        return true;
    }

    raw_score >= parsed_score + 6 && raw_score.saturating_mul(10) >= parsed_score.saturating_mul(11)
}

fn speech_content_score(text: &str) -> usize {
    text.chars()
        .filter(|ch| ch.is_alphanumeric() || !ch.is_ascii() && !ch.is_whitespace())
        .count()
}

fn strip_raw_fallback_noise(input: &str) -> String {
    let without_fenced_code = fenced_code_block_regex().replace_all(input, " ");
    html_tag_regex()
        .replace_all(&without_fenced_code, " ")
        .into_owned()
}

fn strip_streaming_markdown_fragments(input: &str) -> String {
    let mut cleaned = heading_prefix_regex().replace_all(input, "").into_owned();
    cleaned = blockquote_prefix_regex()
        .replace_all(&cleaned, "")
        .into_owned();
    cleaned = list_prefix_regex().replace_all(&cleaned, "").into_owned();
    cleaned = broken_link_regex().replace_all(&cleaned, "$1").into_owned();
    cleaned = broken_bracket_regex()
        .replace_all(&cleaned, "$1")
        .into_owned();
    cleaned = url_regex().replace_all(&cleaned, "").into_owned();
    cleaned = cleaned.replace("```", "");
    cleaned = cleaned.replace("**", "");
    cleaned = cleaned.replace("__", "");
    cleaned = cleaned.replace("~~", "");
    cleaned = cleaned.replace('`', "");
    strip_inline_marker_fragments(&cleaned)
}

fn strip_special_character_noise(input: &str) -> String {
    let mut cleaned = String::with_capacity(input.len());

    for line in input.lines() {
        let sanitized = sanitize_tts_line(line);
        cleaned.push_str(&sanitized);
        cleaned.push('\n');
    }

    empty_punctuation_group_regex()
        .replace_all(&cleaned, " ")
        .into_owned()
}

fn sanitize_tts_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_decorative_separator_line(trimmed) {
        return String::new();
    }

    if looks_like_table_row(trimmed) {
        return flatten_table_row(trimmed);
    }

    trimmed.to_string()
}

fn is_decorative_separator_line(line: &str) -> bool {
    let stripped = line.trim().trim_matches('|').trim();
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|ch| ch.is_whitespace() || matches!(ch, '-' | '_' | '*' | ':' | '=' | '.'))
}

fn looks_like_table_row(line: &str) -> bool {
    line.matches('|').count() >= 2 && !line.contains('$')
}

fn flatten_table_row(line: &str) -> String {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
}

fn strip_inline_marker_fragments(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    let mut in_math = false;

    while index < chars.len() {
        let ch = chars[index];

        if ch == '$' && !is_escaped_char(&chars, index) {
            let run_length = dollar_run_length(&chars, index);
            if in_math || has_matching_dollar_run(&chars, index + run_length, run_length) {
                for _ in 0..run_length {
                    output.push('$');
                }
                in_math = !in_math;
                index += run_length;
                continue;
            }
        }

        if in_math {
            output.push(ch);
            index += 1;
            continue;
        }

        if matches!(ch, '*' | '_' | '~') {
            let previous = index
                .checked_sub(1)
                .and_then(|position| chars.get(position))
                .copied();
            let next = chars.get(index + 1).copied();
            if should_strip_inline_marker(previous, ch, next) {
                index += 1;
                continue;
            }
        }

        output.push(ch);
        index += 1;
    }

    output
}

fn is_escaped_char(chars: &[char], index: usize) -> bool {
    if index == 0 {
        return false;
    }

    let mut slash_count = 0usize;
    let mut position = index;
    while position > 0 {
        position -= 1;
        if chars[position] != '\\' {
            break;
        }
        slash_count += 1;
    }

    slash_count % 2 == 1
}

fn dollar_run_length(chars: &[char], start: usize) -> usize {
    let mut length = 0usize;
    while chars.get(start + length) == Some(&'$') {
        length += 1;
    }
    length
}

fn has_matching_dollar_run(chars: &[char], start: usize, run_length: usize) -> bool {
    let mut index = start;
    while index < chars.len() {
        if chars[index] == '$' && !is_escaped_char(chars, index) {
            let next_run_length = dollar_run_length(chars, index);
            if next_run_length == run_length {
                return true;
            }
            index += next_run_length;
            continue;
        }
        index += 1;
    }
    false
}

fn should_strip_inline_marker(previous: Option<char>, marker: char, next: Option<char>) -> bool {
    previous == Some(marker)
        || next == Some(marker)
        || previous.is_none()
        || next.is_none()
        || previous.is_some_and(is_marker_boundary)
        || next.is_some_and(is_marker_boundary)
}

fn is_marker_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | '"' | '\'' | ':' | ';' | ','
        )
}

fn normalize_tts_text(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut pending_break = false;

    for ch in input.chars() {
        if matches!(ch, '\r' | '\n') {
            pending_break = !normalized.is_empty();
            pending_space = false;
            continue;
        }

        if ch.is_whitespace() {
            if !pending_break {
                pending_space = true;
            }
            continue;
        }

        if is_closing_punctuation(ch) {
            while normalized.ends_with(' ') {
                normalized.pop();
            }
        } else if pending_break {
            trim_trailing_spaces(&mut normalized);
            if !normalized.is_empty() && !normalized.ends_with('\n') {
                normalized.push_str("\n\n");
            }
        } else if pending_space
            && !normalized.is_empty()
            && !normalized.ends_with('\n')
            && !normalized.ends_with(' ')
        {
            normalized.push(' ');
        }

        pending_space = false;
        pending_break = false;
        normalized.push(ch);
    }

    trim_trailing_spaces(&mut normalized);
    normalized.trim().to_string()
}

fn trim_trailing_spaces(value: &mut String) {
    while value.ends_with(' ') {
        value.pop();
    }
}

fn is_closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | '!' | '?' | ':' | ';' | ')' | ']' | '}' | '%'
    )
}

fn heading_prefix_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?m)^\s{0,3}#{1,6}\s*").expect("valid heading regex"))
}

fn blockquote_prefix_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?m)^\s{0,3}>\s?").expect("valid blockquote regex"))
}

fn list_prefix_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"(?m)^\s*(?:[-+*]|\d+\.)\s+").expect("valid list marker regex"))
}

fn broken_link_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^\)]*$").expect("valid broken link regex"))
}

fn broken_bracket_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\[([^\]]+)\]").expect("valid bracket regex"))
}

fn url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"https?://\S+").expect("valid URL regex"))
}

fn fenced_code_block_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?s)```[^\n]*\n.*?(?:```|$)").expect("valid fenced code block regex")
    })
}

fn html_tag_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"</?[^>]+>").expect("valid HTML tag regex"))
}

fn empty_punctuation_group_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\(\s*\)|\[\s*\]|\{\s*\}|<\s*>").expect("valid empty punctuation regex")
    })
}

#[cfg(test)]
mod tests {
    use super::filter_for_tts;

    #[test]
    fn strips_markdown_and_code_blocks_for_tts() {
        let filtered = filter_for_tts(
            "# Heading\n\n**Bold** [docs](https://example.com)\n\n```rust\nfn main() {}\n```\n",
        );

        assert_eq!(filtered, "Heading.\n\nBold docs");
    }

    #[test]
    fn preserves_math_and_cleans_streaming_markers() {
        let filtered = filter_for_tts("**Proof**: $E = mc^2$ and `cargo run`");
        assert_eq!(filtered, "Proof: $E = mc^2$ and cargo run");
    }

    #[test]
    fn handles_partial_markdown_during_streaming() {
        let filtered = filter_for_tts("**Wait [docs](https://example.com");
        assert_eq!(filtered, "Wait docs");
    }

    #[test]
    fn preserves_blockquote_and_bold_text_content() {
        let filtered = filter_for_tts("> **Quoted test**\n> and **bold text** stays spoken");
        assert_eq!(filtered, "Quoted test\n\nand bold text stays spoken");
    }

    #[test]
    fn salvages_partial_quote_and_bold_markdown_text() {
        let filtered = filter_for_tts("> **Quoted test");
        assert_eq!(filtered, "Quoted test");
    }

    #[test]
    fn raw_fallback_does_not_resurrect_fenced_code_blocks() {
        let filtered = filter_for_tts("> **Quoted**\n```rust\nprintln!(\"hi\");\n");
        assert_eq!(filtered, "Quoted");
    }

    #[test]
    fn preserves_language_labels_while_stripping_markdown_markers() {
        let filtered =
            filter_for_tts("**Japanese:**\nこんにちは。\n\n**English:** Hello! I am Amadeus.");
        assert_eq!(
            filtered,
            "Japanese:\n\nこんにちは。\n\nEnglish: Hello! I am Amadeus."
        );
    }

    #[test]
    fn preserves_html_text_and_drops_decorative_separators() {
        let filtered = filter_for_tts("Intro\n\n---\n\n<br>Some HTML here</br>");
        assert_eq!(filtered, "Intro\n\nSome HTML here");
    }

    #[test]
    fn normalizes_complex_markdown_for_tts_without_losing_math() {
        let input = r#"

# How Neural Networks Work

A neural network is a _computational model_ inspired by the brain.

It uses modern techniques.

## Math Behind It

The output is:

$$y = \sum_{i=0}^{n} w_i x_i + b$$

Or inline: $y = Wx + b$

## Code Example

```python
def forward(x):
    return sigmoid(W @ x + b)
```

Use relu(x) as an activation function.

> Note: always normalize your inputs.

- First point
- Second point

| Layer | Output |
|-------|--------|
| Input | 128    |
| Dense | 64     |

Check [this paper](https://arxiv.org/abs/1234) for details.

---

<br>Some HTML here</br>
"#;

        let filtered = filter_for_tts(input);

        assert!(filtered.contains("How Neural Networks Work."));
        assert!(
            filtered.contains("A neural network is a computational model inspired by the brain.")
        );
        assert!(filtered.contains("Math Behind It."));
        assert!(filtered.contains("$$y = \\sum_{i=0}^{n} w_i x_i + b$$"));
        assert!(filtered.contains("Or inline: $y = Wx + b$"));
        assert!(filtered.contains("Use relu(x) as an activation function."));
        assert!(filtered.contains("Note: always normalize your inputs."));
        assert!(filtered.contains("First point"));
        assert!(filtered.contains("Second point"));
        assert!(filtered.contains("Layer Output"));
        assert!(filtered.contains("Input 128"));
        assert!(filtered.contains("Dense 64"));
        assert!(filtered.contains("Check this paper for details."));
        assert!(filtered.contains("Some HTML here"));
        assert!(!filtered.contains("def forward"));
        assert!(!filtered.contains("https://"));
        assert!(!filtered.contains("|-------|--------|"));
        assert!(!filtered.contains("---"));
    }
}
