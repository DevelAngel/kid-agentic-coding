//! Markdown rendering shared by bubble layout and presentation.

use ratatui::text::{Line, Span, Text};

fn preserve_line_breaks(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut rendered = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let is_fence =
            content.trim_start().starts_with("```") || content.trim_start().starts_with("~~~");

        if is_fence {
            in_code_block = !in_code_block;
        }

        if !in_code_block && !is_fence && !content.is_empty() && line.ends_with('\n') {
            let content = content.strip_suffix('\r').unwrap_or(content);
            rendered.push_str(content);
            rendered.push_str("  \n");
        } else {
            rendered.push_str(line);
        }
    }

    rendered
}

pub fn render(text: &str) -> Text<'static> {
    let rendered = preserve_line_breaks(text);
    let text = tui_markdown::from_str(&rendered);
    let lines = text.lines.into_iter().map(|line| {
        Line::from_iter(
            line.spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style)),
        )
    });

    Text::from(lines.collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::preserve_line_breaks;

    #[test]
    fn preserves_single_line_breaks() {
        assert_eq!(preserve_line_breaks("first\nsecond"), "first  \nsecond");
    }

    #[test]
    fn preserves_code_block_line_breaks() {
        assert_eq!(
            preserve_line_breaks("```text\nfirst\nsecond\n```"),
            "```text\nfirst\nsecond\n```"
        );
    }
}
