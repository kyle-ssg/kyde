//! Reformat Code — pretty-print a whole document.
//!
//! JSON is the one language so far (JSONC comments and trailing commas included). It needs no
//! grammar, so it works whether or not the JSON pack is compiled in or installed. Every token is
//! copied verbatim — key order, number spelling and string escapes never change — and only the
//! whitespace between tokens is rewritten, so the output re-parses to the same value.

use crate::Lang;
use std::ops::Range;

/// Why a document couldn't be reformatted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// There's no formatter for this language yet.
    #[error("only JSON files can be reformatted so far")]
    Unsupported,
    /// The document doesn't parse; `line`/`col` are 1-based (`col` counts characters).
    #[error("invalid JSON at line {line}, column {col}: {what}")]
    Invalid {
        /// 1-based line of the offending token.
        line: usize,
        /// 1-based character column of the offending token.
        col: usize,
        /// What was wrong there.
        what: &'static str,
    },
}

/// A reformatted document: replace `range` of the old text with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reformatted {
    /// The byte range of the old text that changes — the first token to the last, so the
    /// whitespace around the document (a trailing newline, say) stays as it was.
    pub range: Range<usize>,
    /// The new text for `range`. Equal to the old text when it was already formatted.
    pub text: String,
    /// The caret passed in, moved to the same spot in the new document (a byte offset into the
    /// whole new text, not into `text`).
    pub caret: usize,
}

/// Reformat `source` as `lang`: JSON is pretty-printed `JSON.stringify(v, null, 2)`-style —
/// one entry per line, `"key": value`, `{}`/`[]` kept compact. The indent unit is the file's
/// own (the smallest indent it already uses, or a tab), else two spaces; CRLF files stay CRLF.
/// Comments stay where they were — a trailing one on its line, a standalone one on its own —
/// and a blank line between entries survives (collapsed to one).
///
/// ```
/// use kyde_syntax::{reformat, FormatError, Lang};
/// let r = reformat(r#"{"a": [1, {}], "b": "x"}"#, Lang::Json, 0).unwrap();
/// assert_eq!(r.text, "{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": \"x\"\n}");
/// assert!(matches!(reformat("{\"a\" 1}", Lang::Json, 0), Err(FormatError::Invalid { .. })));
/// assert_eq!(reformat("fn main() {}", Lang::Rust, 0), Err(FormatError::Unsupported));
/// ```
pub fn reformat(source: &str, lang: Lang, caret: usize) -> Result<Reformatted, FormatError> {
    if !lang.can_reformat() {
        return Err(FormatError::Unsupported);
    }
    let invalid = |at: usize, what| {
        let before = &source[..at];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        FormatError::Invalid {
            line: before.matches('\n').count() + 1,
            col: before[line_start..].chars().count() + 1,
            what,
        }
    };
    let toks = tokenize(source).map_err(|(at, what)| invalid(at, what))?;
    validate(source, &toks).map_err(|(at, what)| invalid(at, what))?;
    let (Some(first), Some(last)) = (toks.first(), toks.last()) else {
        return Err(invalid(source.len(), "expected a value"));
    };
    let range = first.span.start..last.span.end;
    let nl = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let (text, mapped) = print(source, &toks, &indent_unit(source), nl, caret);
    let caret = if caret < range.start {
        caret
    } else {
        // Past the last token: keep its distance from the end.
        mapped.unwrap_or(range.start + text.len() + caret.saturating_sub(range.end))
    };
    Ok(Reformatted { range, text, caret })
}

impl Lang {
    /// Whether [`reformat`] has a formatter for this language — cheap, for gating menu items.
    #[must_use]
    pub fn can_reformat(self) -> bool {
        self == Lang::Json
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open(u8),
    Close(u8),
    Colon,
    Comma,
    /// A string, number, `true`/`false`/`null` — anything copied as one word.
    Value,
    LineComment,
    BlockComment,
}

struct Tok {
    kind: Kind,
    span: Range<usize>,
    /// Newlines in the whitespace before this token, capped at 2 (= a blank line).
    newlines_before: u8,
}

/// Split `src` into tokens. Strings are scanned (escapes honoured) but not validated beyond
/// ending on their own line; bare words are checked later by [`validate`].
fn tokenize(src: &str) -> Result<Vec<Tok>, (usize, &'static str)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let mut newlines = 0u8;
    while let Some(&c) = b.get(i) {
        let (kind, end) = match c {
            b'\n' => {
                newlines = newlines.saturating_add(1).min(2);
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'{' | b'[' => (Kind::Open(c), i + 1),
            b'}' | b']' => (Kind::Close(c), i + 1),
            b':' => (Kind::Colon, i + 1),
            b',' => (Kind::Comma, i + 1),
            b'"' => {
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None | Some(b'\n') => return Err((i, "unterminated string")),
                        Some(b'\\') => j += 2,
                        Some(b'"') => break (Kind::Value, j + 1),
                        Some(_) => j += 1,
                    }
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = src[i..].find('\n').map_or(src.len(), |n| i + n);
                let end = end - usize::from(src[i..end].ends_with('\r'));
                (Kind::LineComment, end)
            }
            b'/' if b.get(i + 1) == Some(&b'*') => match src[i + 2..].find("*/") {
                Some(n) => (Kind::BlockComment, i + 2 + n + 2),
                None => return Err((i, "unterminated comment")),
            },
            b'/' => return Err((i, "unexpected '/'")),
            _ => {
                let len = src[i..]
                    .find(|c: char| c.is_ascii_whitespace() || "{}[]:,\"/".contains(c))
                    .unwrap_or(src.len() - i);
                (Kind::Value, i + len)
            }
        };
        out.push(Tok {
            kind,
            span: i..end,
            newlines_before: newlines,
        });
        newlines = 0;
        i = end;
    }
    Ok(out)
}

/// Check the tokens (comments aside) form exactly one JSON value. Trailing commas are allowed
/// (JSONC — `tsconfig.json` and friends). Iterative, so deep nesting can't overflow the stack.
fn validate(src: &str, toks: &[Tok]) -> Result<(), (usize, &'static str)> {
    #[derive(PartialEq)]
    enum Want {
        Value,
        ValueOrClose,
        KeyOrClose,
        Colon,
        After,
    }
    let mut stack: Vec<u8> = Vec::new();
    let mut want = Want::Value;
    for t in toks.iter().filter(|t| !t.is_comment()) {
        let at = t.span.start;
        let word = &src[t.span.clone()];
        want = match (want, t.kind) {
            (Want::ValueOrClose, Kind::Close(b']')) | (Want::KeyOrClose, Kind::Close(b'}')) => {
                stack.pop();
                Want::After
            }
            (Want::Value | Want::ValueOrClose, Kind::Open(c)) => {
                stack.push(c);
                if c == b'{' {
                    Want::KeyOrClose
                } else {
                    Want::ValueOrClose
                }
            }
            (Want::Value | Want::ValueOrClose, Kind::Value) if is_scalar(word) => Want::After,
            (Want::Value | Want::ValueOrClose, _) => return Err((at, "expected a value")),
            (Want::KeyOrClose, Kind::Value) if word.starts_with('"') => Want::Colon,
            (Want::KeyOrClose, _) => return Err((at, "expected a \"key\" or '}'")),
            (Want::Colon, Kind::Colon) => Want::Value,
            (Want::Colon, _) => return Err((at, "expected ':'")),
            (Want::After, kind) => match (stack.last(), kind) {
                (None, _) => return Err((at, "unexpected text after the document")),
                (Some(b'{'), Kind::Comma) => Want::KeyOrClose,
                (Some(_), Kind::Comma) => Want::ValueOrClose,
                (Some(&open), Kind::Close(close)) if close == open + 2 => {
                    // `{`+2 = `}`, `[`+2 = `]` in ASCII.
                    stack.pop();
                    Want::After
                }
                (Some(b'{'), _) => return Err((at, "expected ',' or '}'")),
                (Some(_), _) => return Err((at, "expected ',' or ']'")),
            },
        };
    }
    if want == Want::After && stack.is_empty() {
        Ok(())
    } else if want == Want::Value && stack.is_empty() {
        Err((src.len(), "expected a value"))
    } else {
        Err((src.len(), "unexpected end of document"))
    }
}

impl Tok {
    fn is_comment(&self) -> bool {
        matches!(self.kind, Kind::LineComment | Kind::BlockComment)
    }
}

/// A string, `true`/`false`/`null`, or a JSON number.
fn is_scalar(w: &str) -> bool {
    if w.starts_with('"') || matches!(w, "true" | "false" | "null") {
        return true;
    }
    // -?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?
    let digits = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let s = w.strip_prefix('-').unwrap_or(w);
    let int = digits(s);
    if int == 0 || (int > 1 && s.starts_with('0')) {
        return false;
    }
    let mut s = &s[int..];
    if let Some(frac) = s.strip_prefix('.') {
        let n = digits(frac);
        if n == 0 {
            return false;
        }
        s = &frac[n..];
    }
    if let Some(exp) = s.strip_prefix(['e', 'E']) {
        let exp = exp.strip_prefix(['+', '-']).unwrap_or(exp);
        let n = digits(exp);
        if n == 0 {
            return false;
        }
        s = &exp[n..];
    }
    s.is_empty()
}

/// The file's indent unit: a tab if its first indented line uses one, else the smallest
/// space indent it uses (block-comment `*` lines aside), else two spaces.
fn indent_unit(src: &str) -> String {
    let mut min = usize::MAX;
    for line in src.lines() {
        let rest = line.trim_start_matches([' ', '\t']);
        let lead = line.len() - rest.len();
        if lead == 0 || rest.trim_end().is_empty() || rest.starts_with('*') {
            continue;
        }
        if line.starts_with('\t') {
            return "\t".into();
        }
        min = min.min(lead);
    }
    if min == usize::MAX {
        "  ".into()
    } else {
        " ".repeat(min.min(8))
    }
}

/// Lay the (validated) tokens out. Returns the text for the tokens' span, plus where `caret`
/// lands in the new document when it sat inside that span.
fn print(src: &str, toks: &[Tok], unit: &str, nl: &str, caret: usize) -> (String, Option<usize>) {
    let base = toks.first().map_or(0, |t| t.span.start);
    let mut out = String::with_capacity(src.len() + src.len() / 2);
    let mut mapped = None;
    let mut depth = 0usize;
    let mut need_break = false;
    let mut need_space = false;
    let mut prev: Option<Kind> = None;
    let brk = |out: &mut String, depth: usize, blank: bool| {
        if blank {
            out.push_str(nl);
        }
        out.push_str(nl);
        for _ in 0..depth {
            out.push_str(unit);
        }
    };
    // Emit one token, moving the caret with it if it sat in or just before it.
    let mut emit = |out: &mut String, t: &Tok| {
        if mapped.is_none() && caret <= t.span.end {
            mapped = Some(base + out.len() + caret.saturating_sub(t.span.start));
        }
        out.push_str(&src[t.span.clone()]);
    };
    let mut i = 0;
    while let Some(t) = toks.get(i) {
        i += 1;
        if let Kind::Close(_) = t.kind {
            depth = depth.saturating_sub(1);
            brk(&mut out, depth, false);
            emit(&mut out, t);
            (need_break, need_space) = (false, false);
            prev = Some(t.kind);
            continue;
        }
        let comment = t.is_comment();
        if comment && prev.is_some() && t.newlines_before == 0 {
            out.push(' '); // a trailing comment stays on its line
        } else if need_break || (comment && prev.is_some()) {
            let blank = t.newlines_before >= 2 && !matches!(prev, Some(Kind::Open(_)));
            brk(&mut out, depth, blank);
            need_break = false;
        } else if need_space {
            out.push(' ');
        }
        need_space = false;
        emit(&mut out, t);
        match t.kind {
            Kind::Open(_) => match toks.get(i) {
                Some(close) if matches!(close.kind, Kind::Close(_)) => {
                    emit(&mut out, close); // `{}` / `[]` stay compact
                    i += 1;
                    prev = Some(close.kind);
                    continue;
                }
                _ => {
                    depth += 1;
                    need_break = true;
                }
            },
            Kind::Colon => need_space = true,
            Kind::Comma | Kind::LineComment => need_break = true,
            Kind::BlockComment if t.newlines_before > 0 || prev.is_none() => need_break = true,
            Kind::BlockComment => need_space = true,
            Kind::Value | Kind::Close(_) => {}
        }
        prev = Some(t.kind);
    }
    (out, mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        let r = reformat(src, Lang::Json, 0).unwrap();
        format!("{}{}{}", &src[..r.range.start], r.text, &src[r.range.end..])
    }

    /// The one-line blob from the editor becomes one entry per line, every token verbatim.
    #[test]
    fn pretty_prints_a_one_liner() {
        let src = r#"{"type":"service_account","n":-1.5e3,"esc":"a\"}{b","list":[1,[],{"x":null}],"o":{}}"#;
        assert_eq!(
            fmt(src),
            r#"{
  "type": "service_account",
  "n": -1.5e3,
  "esc": "a\"}{b",
  "list": [
    1,
    [],
    {
      "x": null
    }
  ],
  "o": {}
}"#
        );
    }

    /// A hand-kept JSONC file: its 4-space indent, CRLF endings, blank line, comments and
    /// trailing comma survive, and formatting it again changes nothing.
    #[test]
    fn keeps_the_files_own_style() {
        let src = "// settings\r\n{\r\n    \"a\": 1, // one\r\n\r\n    /* two */\r\n    \"b\": [2,],\r\n}\r\n";
        let out = fmt(src);
        assert_eq!(
            out,
            "// settings\r\n{\r\n    \"a\": 1, // one\r\n\r\n    /* two */\r\n    \"b\": [\r\n        2,\r\n    ],\r\n}\r\n"
        );
        assert_eq!(fmt(&out), out, "formatting is idempotent");
    }

    /// The caret stays on the token it was on.
    #[test]
    fn caret_follows_its_token() {
        let src = r#"{"a":1,"bc":2}"#;
        let r = reformat(src, Lang::Json, src.find("c\"").unwrap()).unwrap();
        assert_eq!(&r.text[r.caret..], "c\": 2\n}");
    }

    /// Broken JSON is refused with where it broke, not half-formatted.
    #[test]
    fn rejects_invalid_json_with_its_position() {
        let err = |s| reformat(s, Lang::Json, 0).unwrap_err().to_string();
        assert_eq!(
            err("{\n  \"a\": 1\n  \"b\": 2\n}"),
            "invalid JSON at line 3, column 3: expected ',' or '}'"
        );
        assert_eq!(
            err("{\"a\": undefined}"),
            "invalid JSON at line 1, column 7: expected a value"
        );
        assert_eq!(
            err("[1, 2"),
            "invalid JSON at line 1, column 6: unexpected end of document"
        );
    }
}
