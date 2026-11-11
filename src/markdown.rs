use iced::Element;
use iced::widget::markdown::{self, Content};

use crate::app::Message;
use crate::theme::ThemeMode;

pub(super) fn parse(source: &str) -> Content {
    Content::parse(source)
}

pub(super) fn append(content: &mut Content, chunk: &str) {
    content.push_str(chunk);
}

pub(super) fn view<'a>(content: &'a Content, theme_mode: ThemeMode) -> Element<'a, Message> {
    let mut settings = markdown::Settings::with_text_size(16, theme_mode.iced_theme());
    settings.code_size = 13.0.into();
    settings.spacing = 14.0.into();
    markdown::view(content.items(), settings)
        .map(Message::MarkdownLinkClicked)
        .into()
}

pub(super) fn open_link(uri: &str) -> Result<(), &'static str> {
    let url = safe_url(uri)?;

    webbrowser::open(url.as_str()).map_err(|_| "could not open link in the default browser")
}

fn safe_url(uri: &str) -> Result<url::Url, &'static str> {
    let url = url::Url::parse(uri).map_err(|_| "invalid URL")?;
    if matches!(url.scheme(), "http" | "https") {
        Ok(url)
    } else {
        Err("only HTTP and HTTPS links are supported")
    }
}

#[cfg(test)]
mod tests {
    use super::{append, parse, safe_url};
    use iced::widget::markdown::Item;

    #[test]
    fn parses_headings_emphasis_and_lists() {
        let content = parse(
            "# Heading\n\n**Bold**, *italic*, and `inline code`.\n\n- first\n  - nested\n- second\n\n1. one\n2. two\n\n> quoted\n\n---\n\n```rust\nfn main() {}\n```",
        );

        assert!(matches!(content.items()[0], Item::Heading(..)));
        assert!(matches!(content.items()[1], Item::Paragraph(..)));
        assert!(matches!(content.items()[2], Item::List { .. }));
        assert!(matches!(content.items()[3], Item::List { .. }));
        assert!(matches!(content.items()[4], Item::Quote(..)));
        assert!(matches!(content.items()[5], Item::Rule));
        assert!(matches!(content.items()[6], Item::CodeBlock { .. }));
    }

    #[test]
    fn incrementally_parses_fenced_code_without_requiring_a_closed_fence() {
        let mut content = parse("```rust\nfn main() {");
        assert!(matches!(
            content.items(),
            [Item::CodeBlock { code, .. }] if code.contains("fn main()")
        ));

        append(&mut content, "\n    println!(\"hi\");\n}\n```");
        assert!(matches!(content.items(), [Item::CodeBlock { .. }]));
    }

    #[test]
    fn parses_mixed_arabic_and_english_markdown() {
        let content = parse("مرحبًا بالعالم — Hello, **world**!");
        assert!(matches!(content.items(), [Item::Paragraph(..)]));
    }

    #[test]
    fn only_allows_http_and_https_links() {
        assert!(safe_url("javascript:alert(1)").is_err());
        assert!(safe_url("file:///etc/passwd").is_err());
        assert!(safe_url("https://example.com").is_ok());
    }
}
