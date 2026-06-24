//! Minimal Markdown → block model for the side-by-side preview (the markdown plugin).
//! Parses with `pulldown-cmark` into a flat list of blocks with inline styling; `render.rs`
//! turns these into gpui elements. Pure (no gpui) so it's unit-testable.

/// An inline run of text with its active emphasis.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

/// A block-level element of the rendered document.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    /// Fenced/indented code block (raw text, monospace).
    Code(String),
    /// List item with its nesting depth (1 = top level).
    ListItem(usize, Vec<Span>),
    Quote(Vec<Span>),
    Rule,
    /// An image. `src` is the raw URL or (file-relative) path from the source;
    /// `alt` is the alt text. Covers both Markdown `![alt](src)` and raw HTML
    /// `<img src="…" alt="…">` (the latter is how the README embeds screenshots).
    Image {
        src: String,
        alt: String,
    },
}

/// Parse Markdown source into a flat list of blocks.
pub fn parse(src: &str) -> Vec<Block> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    let mut out = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let (mut bold, mut italic) = (0i32, 0i32);
    let mut heading: Option<u8> = None;
    let mut in_code_block = false;
    let mut code_buf = String::new();
    let mut in_item = false;
    let mut depth = 0usize;
    let mut quote = false;
    let mut in_image = false;
    let mut img_src = String::new();
    let mut img_alt = String::new();

    let push = |spans: &mut Vec<Span>, text: &str, bold: i32, italic: i32, code: bool| {
        if text.is_empty() {
            return;
        }
        spans.push(Span {
            text: text.to_string(),
            bold: bold > 0,
            italic: italic > 0,
            code,
        });
    };

    for ev in Parser::new(src) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(level_to_u8(level));
            }
            Event::End(TagEnd::Heading(_)) => {
                out.push(Block::Heading(
                    heading.take().unwrap_or(1),
                    std::mem::take(&mut spans),
                ));
            }
            Event::End(TagEnd::Paragraph) => {
                if in_item {
                    // keep accumulating into the item; flushed at End(Item)
                } else if quote {
                    out.push(Block::Quote(std::mem::take(&mut spans)));
                } else if !spans.is_empty() {
                    // Skip empty paragraphs — e.g. a paragraph that held only an
                    // image (whose alt text was routed away, leaving no spans).
                    out.push(Block::Paragraph(std::mem::take(&mut spans)));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                out.push(Block::Code(
                    std::mem::take(&mut code_buf).trim_end().to_string(),
                ));
            }
            Event::Start(Tag::List(_)) => depth += 1,
            Event::End(TagEnd::List(_)) => depth = depth.saturating_sub(1),
            Event::Start(Tag::Item) => in_item = true,
            Event::End(TagEnd::Item) => {
                in_item = false;
                out.push(Block::ListItem(depth.max(1), std::mem::take(&mut spans)));
            }
            Event::Start(Tag::BlockQuote(_)) => quote = true,
            Event::End(TagEnd::BlockQuote(_)) => quote = false,
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold -= 1,
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic -= 1,
            Event::Start(Tag::Image { dest_url, .. }) => {
                in_image = true;
                img_src = dest_url.to_string();
                img_alt.clear();
            }
            Event::End(TagEnd::Image) => {
                in_image = false;
                out.push(Block::Image {
                    src: std::mem::take(&mut img_src),
                    alt: std::mem::take(&mut img_alt),
                });
            }
            // Raw HTML — the README embeds screenshots as `<img>` inside `<p>`, not
            // Markdown image syntax. Pull any `<img src=…>` out and render it.
            Event::Html(s) | Event::InlineHtml(s) => {
                for (src, alt) in extract_imgs(&s) {
                    out.push(Block::Image { src, alt });
                }
            }
            Event::Code(s) => push(&mut spans, &s, bold, italic, true),
            Event::Text(s) => {
                if in_code_block {
                    code_buf.push_str(&s);
                } else if in_image {
                    img_alt.push_str(&s);
                } else {
                    push(&mut spans, &s, bold, italic, false);
                }
            }
            Event::SoftBreak => push(&mut spans, " ", bold, italic, false),
            Event::HardBreak => push(&mut spans, "\n", bold, italic, false),
            Event::Rule => out.push(Block::Rule),
            _ => {}
        }
    }
    out
}

/// Pull `(src, alt)` out of every `<img …>` tag in a chunk of raw HTML. A tiny
/// hand-rolled scan (no HTML-parser dependency) — handles the common README
/// pattern `<p align="center"><img src="…" alt="…"></p>`. A tag split across two
/// HTML events isn't stitched back together (rare; fine for a live preview).
fn extract_imgs(html: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<img") {
        let start = from + rel;
        let end = lower[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let tag = &html[start..end];
        if let Some(src) = attr(tag, "src") {
            out.push((src, attr(tag, "alt").unwrap_or_default()));
        }
        from = end.max(start + 4);
    }
    out
}

/// Value of `name="…"` / `name='…'` / unquoted `name=…` in an HTML tag
/// (case-insensitive name). Boundary-checked so `data-src=` doesn't match `src=`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{name}=");
    let mut search = 0;
    loop {
        let rel = lower[search..].find(&key)?;
        let at = search + rel;
        let after = at + key.len();
        let boundary_ok = tag[..at]
            .chars()
            .last()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-');
        if !boundary_ok {
            search = after;
            continue;
        }
        let rest = &tag[after..];
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let val = &rest[1..];
            let endq = val.find(quote)?;
            return Some(val[..endq].to_string());
        }
        let endv = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        return Some(rest[..endv].to_string());
    }
}

fn level_to_u8(level: pulldown_cmark::HeadingLevel) -> u8 {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_emphasis_and_code() {
        let blocks = parse("# Title\n\nHello **bold** and `code`.\n\n```\nfn x() {}\n```\n");
        assert_eq!(
            blocks[0],
            Block::Heading(1, vec![span("Title", false, false, false)])
        );
        // Paragraph with a bold run and an inline-code run.
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("expected paragraph, got {:?}", blocks[1]);
        };
        assert!(spans.iter().any(|s| s.text == "bold" && s.bold));
        assert!(spans.iter().any(|s| s.text == "code" && s.code));
        assert_eq!(blocks[2], Block::Code("fn x() {}".to_string()));
    }

    #[test]
    fn parses_list_items() {
        let blocks = parse("- one\n- two\n");
        let items: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem(..)))
            .collect();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn parses_markdown_image() {
        let blocks = parse("![a cat](cat.png)\n");
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Image { src, alt } if src == "cat.png" && alt == "a cat")));
    }

    #[test]
    fn extracts_html_img_from_centered_paragraph() {
        let blocks = parse(
            "<p align=\"center\">\n  <img src=\"assets/x.png\" alt=\"X\" width=\"900\">\n</p>\n",
        );
        let imgs: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { src, alt } => Some((src.as_str(), alt.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(imgs, vec![("assets/x.png", "X")]);
    }

    #[test]
    fn img_extractor_handles_quotes_case_and_data_src() {
        assert_eq!(
            extract_imgs("<img src='a.png'>"),
            vec![("a.png".to_string(), String::new())]
        );
        assert_eq!(
            extract_imgs("<IMG SRC=\"b.png\" ALT=\"hi\">"),
            vec![("b.png".to_string(), "hi".to_string())]
        );
        // `data-src` must not be mistaken for `src`.
        assert_eq!(
            extract_imgs("<img data-src=\"x\" src=\"real.png\">"),
            vec![("real.png".to_string(), String::new())]
        );
        // Two images in one HTML chunk.
        assert_eq!(
            extract_imgs("<img src=\"1.png\"><img src=\"2.png\">").len(),
            2
        );
    }

    fn span(t: &str, b: bool, i: bool, c: bool) -> Span {
        Span {
            text: t.to_string(),
            bold: b,
            italic: i,
            code: c,
        }
    }
}
