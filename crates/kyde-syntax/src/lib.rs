#![deny(missing_docs)]
//! Tree-sitter syntax highlighting, the way Zed does it: parse with a grammar,
//! run the grammar's `HIGHLIGHTS_QUERY`, map capture names → theme colors.
//!
//! This is plain Rust (no UI framework) and unit-testable. It turns source text into a
//! flat list of colored spans the UI can render.

use kyde_theme as theme;
use std::path::Path;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// The capture names we recognize. Order matters: indices are referenced by the
/// `HighlightConfiguration`. Anything a grammar emits outside this set falls back
/// to the default foreground.
const CAPTURES: &[&str] = &[
    "keyword",
    "string",
    "string.escape",
    "string.special",
    "number",
    "comment",
    "function",
    "function.method",
    "type",
    "constant",
    "constant.builtin",
    "property",
    "attribute",
    "tag",
    "variable",
    "variable.parameter",
    "operator",
    "punctuation",
    "punctuation.delimiter",
    "punctuation.special",
    "punctuation.bracket",
    // Markdown (block) + CSS at-rules
    "text.title",
    "text.literal",
    "text.reference",
    "text.uri",
    "embedded",
    "charset",
    "import",
    "keyframes",
    "media",
    "supports",
    "namespace",
];

fn capture_color(t: &theme::Theme, name: &str) -> kyde_color::Color {
    match name {
        "keyword" | "charset" | "import" | "keyframes" | "media" | "supports" | "namespace" => {
            t.syn_keyword
        }
        "string" | "string.escape" | "string.special" | "text.literal" => t.syn_string,
        "number" | "constant.builtin" => t.syn_number,
        "comment" => t.syn_comment,
        "function" | "function.method" => t.syn_function,
        // class/type names + markdown headings/links render like declarations in Islands
        "type" | "text.title" | "text.reference" | "text.uri" | "tag" => t.syn_function,
        "constant" => t.syn_constant,
        "property" | "attribute" => t.syn_field,
        "operator"
        | "punctuation"
        | "punctuation.delimiter"
        | "punctuation.special"
        | "punctuation.bracket" => t.syn_operator,
        _ => t.syn_identifier,
    }
}

/// One styled run of text (byte range into the source + its color).
#[derive(Debug, Clone)]
pub struct Span {
    /// Byte offset where the run starts (into the source).
    pub start: usize,
    /// Byte offset where the run ends (exclusive).
    pub end: usize,
    /// The run's colour, resolved from the active theme.
    pub color: kyde_color::Color,
}

/// A language Kyde can recognise (drives highlighting + the install banner). `PlainText`
/// means no grammar — the file renders uncoloured.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lang {
    /// TypeScript + JSX (`.tsx`).
    Tsx,
    /// TypeScript (`.ts`).
    Ts,
    /// JavaScript (`.js`/`.jsx`/`.mjs`/`.cjs`).
    Js,
    /// Rust (`.rs`).
    Rust,
    /// JSON (`.json`).
    Json,
    /// Markdown (`.md`) — block-level only.
    Markdown,
    /// Shell script (`.sh`/`.bash`/`.zsh`).
    Bash,
    /// CSS (`.css`).
    Css,
    /// SCSS (`.scss`) — reuses the CSS grammar.
    Scss,
    /// YAML (`.yml`/`.yaml`).
    Yaml,
    /// TOML (`.toml`).
    Toml,
    /// Python (`.py`/`.pyi`).
    Python,
    /// HTML (`.html`/`.htm`).
    Html,
    /// Go (`.go`).
    Go,
    /// R (`.r`/`.R`).
    R,
    /// LaTeX (`.tex`/`.sty`/`.cls`).
    Latex,
    /// `.env` files — builtin line highlighter (no grammar).
    Env,
    /// `.gitignore` files — builtin line highlighter (no grammar).
    Gitignore,
    /// No grammar; rendered as plain, uncoloured text.
    PlainText,
}

/// Hand-written highlights query for LaTeX — the `tree-sitter-latex` crate ships
/// none (its `HIGHLIGHTS_QUERY` const is commented out). Conservative: colors
/// command names, comments, operators/delimiters, and cross-references; every
/// node kind referenced is a real named node in the grammar's node-types.json.
#[cfg(feature = "latex")]
const LATEX_HIGHLIGHTS: &str = r#"
(command_name) @function
[(comment) (line_comment) (block_comment)] @comment
(operator) @operator
[(delimiter) (math_delimiter)] @punctuation
[(label_reference) (label_definition) (citation)] @text.reference
(placeholder) @variable.parameter
"#;

/// An installable language pack — the unit the user opts into ("plugin").
/// Highlighting for a `Lang` only runs once its pack is installed; until then
/// the file renders as plain text (the whole point: nothing is parsed by
/// default, so opening files stays fast).
#[derive(Clone, Copy)]
pub struct Pack {
    /// Stable id, persisted in plugins.json.
    pub id: &'static str,
    /// Human label shown in the install banner.
    pub name: &'static str,
}

/// All language packs the *this build* can install, in display order. Each entry
/// is gated to its grammar's Cargo feature, so a feature-trimmed build never
/// offers (via the install banner) a pack whose grammar isn't compiled in.
/// `scss` rides the `css` feature (shared grammar); `env`/`gitignore` are
/// builtin line-highlighters with no grammar, so they're always present.
pub const PACKS: &[Pack] = &[
    #[cfg(feature = "json")]
    Pack {
        id: "json",
        name: "JSON",
    },
    #[cfg(feature = "typescript")]
    Pack {
        id: "typescript",
        name: "TypeScript",
    },
    #[cfg(feature = "javascript")]
    Pack {
        id: "javascript",
        name: "JavaScript",
    },
    #[cfg(feature = "rust")]
    Pack {
        id: "rust",
        name: "Rust",
    },
    #[cfg(feature = "markdown")]
    Pack {
        id: "markdown",
        name: "Markdown",
    },
    #[cfg(feature = "shell")]
    Pack {
        id: "shell",
        name: "Shell script",
    },
    #[cfg(feature = "css")]
    Pack {
        id: "css",
        name: "CSS",
    },
    #[cfg(feature = "css")]
    Pack {
        id: "scss",
        name: "SCSS",
    },
    #[cfg(feature = "yaml")]
    Pack {
        id: "yaml",
        name: "YAML",
    },
    #[cfg(feature = "toml")]
    Pack {
        id: "toml",
        name: "TOML",
    },
    #[cfg(feature = "python")]
    Pack {
        id: "python",
        name: "Python",
    },
    #[cfg(feature = "html")]
    Pack {
        id: "html",
        name: "HTML",
    },
    #[cfg(feature = "go")]
    Pack {
        id: "go",
        name: "Go",
    },
    #[cfg(feature = "r")]
    Pack { id: "r", name: "R" },
    #[cfg(feature = "latex")]
    Pack {
        id: "latex",
        name: "LaTeX",
    },
    // Not a language grammar: enables previewing opened font files in their own typeface.
    Pack {
        id: "font",
        name: "Font preview",
    },
    // NOTE: env / gitignore are intentionally NOT here — they're always-on builtin
    // line-highlighters (no grammar, nothing to install), so they never appear in the
    // plugin manager and always highlight. See `Lang::pack` returning `None` for them.
];

fn pack(id: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|p| p.id == id)
}

impl Lang {
    /// Infer the language from a file's name/extension (dotfiles like `.env` first).
    ///
    /// ```
    /// use std::path::Path;
    /// use kyde_syntax::Lang;
    /// assert_eq!(Lang::from_path(Path::new("src/main.rs")), Lang::Rust);
    /// assert_eq!(Lang::from_path(Path::new(".gitignore")), Lang::Gitignore);
    /// ```
    pub fn from_path(path: &Path) -> Self {
        // Filename-based types first (dotfiles have no `extension()`).
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name == ".gitignore" || name.ends_with(".gitignore") {
                return Lang::Gitignore;
            }
            if name == ".env" || name.starts_with(".env.") {
                return Lang::Env;
            }
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("tsx") => Lang::Tsx,
            Some("ts") => Lang::Ts,
            Some("js" | "jsx" | "mjs" | "cjs") => Lang::Js,
            Some("rs") => Lang::Rust,
            Some("json") => Lang::Json,
            Some("md" | "markdown") => Lang::Markdown,
            Some("sh" | "bash" | "zsh") => Lang::Bash,
            Some("css") => Lang::Css,
            Some("scss") => Lang::Scss,
            Some("yml" | "yaml") => Lang::Yaml,
            Some("toml") => Lang::Toml,
            Some("py" | "pyi") => Lang::Python,
            Some("html" | "htm") => Lang::Html,
            Some("go") => Lang::Go,
            Some("r" | "R") => Lang::R,
            Some("tex" | "sty" | "cls" | "latex") => Lang::Latex,
            _ => Lang::PlainText,
        }
    }

    /// The installable pack that provides highlighting for this language, if any.
    /// `PlainText` (and any unknown type) has no pack — no banner, no highlight.
    pub fn pack(self) -> Option<&'static Pack> {
        let id = match self {
            Lang::Tsx | Lang::Ts => "typescript",
            Lang::Js => "javascript",
            Lang::Rust => "rust",
            Lang::Json => "json",
            Lang::Markdown => "markdown",
            Lang::Bash => "shell",
            Lang::Css => "css",
            Lang::Scss => "scss",
            Lang::Yaml => "yaml",
            Lang::Toml => "toml",
            Lang::Python => "python",
            Lang::Html => "html",
            Lang::Go => "go",
            Lang::R => "r",
            Lang::Latex => "latex",
            // Env / Gitignore are always-on builtin line-highlighters (no grammar, nothing to
            // install), so they have no installable pack — `None` means `effective_lang` never
            // gates them to PlainText and no install banner ever shows for them.
            Lang::Env | Lang::Gitignore | Lang::PlainText => return None,
        };
        pack(id)
    }

    // In a zero-grammar build every match arm below is `cfg`'d out, so the match always
    // `return None`s and everything after it is unreachable + `lang` unused. These allows are
    // no-ops in any build with ≥1 grammar (the normal case); they only silence that degenerate
    // config so `cargo build --no-default-features` is warning-free.
    #[allow(unreachable_code, unused_variables)]
    fn config(self) -> Option<HighlightConfiguration> {
        // Each arm is gated to its grammar's Cargo feature. In a feature-trimmed
        // build the absent arms vanish and the lang falls through to the catch-all
        // `_ => return None` (= PlainText), reusing the exact same path as an
        // un-installed pack. The catch-all also covers the builtin-highlighter
        // langs (Env/Gitignore) and PlainText, so the match stays exhaustive
        // regardless of which features are on.
        // Explicit type: in a zero-grammar build every value-producing arm is `cfg`'d out,
        // leaving only the diverging `_ => return None`, so nothing else pins the tuple type.
        #[allow(unreachable_patterns)]
        let (lang, highlights, injections, locals): (
            tree_sitter::Language,
            &str,
            &str,
            &str,
        ) = match self {
            #[cfg(feature = "typescript")]
            Lang::Tsx => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            #[cfg(feature = "typescript")]
            Lang::Ts => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            #[cfg(feature = "javascript")]
            Lang::Js => (
                tree_sitter_javascript::LANGUAGE.into(),
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            ),
            #[cfg(feature = "rust")]
            Lang::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            ),
            #[cfg(feature = "json")]
            Lang::Json => (
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            // Markdown: block grammar only (headings, code fences, lists, quotes).
            // Inline emphasis/links need the separate inline grammar — skipped for now.
            #[cfg(feature = "markdown")]
            Lang::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                "",
                "",
            ),
            #[cfg(feature = "shell")]
            Lang::Bash => (
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "css")]
            Lang::Css => (
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            // SCSS reuses the CSS grammar — covers selectors/properties/values.
            // SCSS-only syntax ($vars, nesting, @mixin) degrades gracefully.
            #[cfg(feature = "css")]
            Lang::Scss => (
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "yaml")]
            Lang::Yaml => (
                tree_sitter_yaml::LANGUAGE.into(),
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "toml")]
            Lang::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "python")]
            Lang::Python => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            // HTML injections (embedded JS/CSS) skipped — block grammar only, like Markdown.
            #[cfg(feature = "html")]
            Lang::Html => (
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "go")]
            Lang::Go => (
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            #[cfg(feature = "r")]
            Lang::R => (
                tree_sitter_r::LANGUAGE.into(),
                tree_sitter_r::HIGHLIGHTS_QUERY,
                "",
                tree_sitter_r::LOCALS_QUERY,
            ),
            // LaTeX grammar ships no highlights query — use our hand-written one.
            #[cfg(feature = "latex")]
            Lang::Latex => (tree_sitter_latex::LANGUAGE.into(), LATEX_HIGHLIGHTS, "", ""),
            // Builtin-highlighter langs, PlainText, and any lang whose grammar
            // feature isn't compiled in → no tree-sitter config.
            _ => return None,
        };
        // tree-sitter-typescript's HIGHLIGHTS_QUERY only holds TS-specific rules and
        // inherits the base ECMAScript highlighting from the JS grammar — without
        // prepending it, TS/TSX matches no captures and renders as plain text.
        let highlights_owned: String = match self {
            #[cfg(feature = "typescript")]
            Lang::Tsx | Lang::Ts => {
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    highlights
                )
            }
            _ => highlights.to_string(),
        };
        let mut cfg =
            HighlightConfiguration::new(lang, "kyde", &highlights_owned, injections, locals)
                .ok()?;
        cfg.configure(CAPTURES);
        Some(cfg)
    }
}

impl Lang {
    /// The raw tree-sitter grammar for this language (no highlight config), used
    /// for structural analysis like code folding. `None` for the builtin
    /// line-highlighters and `PlainText`.
    // Zero-grammar build: every arm is `cfg`'d out → the match always `return None`s, so
    // `Some(lang)` is unreachable + `lang` unused. No-op allows in any build with ≥1 grammar.
    #[allow(unreachable_code, unused_variables)]
    fn grammar(self) -> Option<tree_sitter::Language> {
        // Feature-gated like `config()`: an absent grammar's arm vanishes and the
        // lang falls through to `_ => return None` (no folding, like PlainText).
        #[allow(unreachable_patterns)]
        let lang: tree_sitter::Language = match self {
            #[cfg(feature = "typescript")]
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            #[cfg(feature = "typescript")]
            Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "javascript")]
            Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "rust")]
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "json")]
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            #[cfg(feature = "markdown")]
            Lang::Markdown => tree_sitter_md::LANGUAGE.into(),
            #[cfg(feature = "shell")]
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            #[cfg(feature = "css")]
            Lang::Css | Lang::Scss => tree_sitter_css::LANGUAGE.into(),
            #[cfg(feature = "yaml")]
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            #[cfg(feature = "toml")]
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            #[cfg(feature = "python")]
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "html")]
            Lang::Html => tree_sitter_html::LANGUAGE.into(),
            #[cfg(feature = "go")]
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            #[cfg(feature = "r")]
            Lang::R => tree_sitter_r::LANGUAGE.into(),
            #[cfg(feature = "latex")]
            Lang::Latex => tree_sitter_latex::LANGUAGE.into(),
            _ => return None,
        };
        Some(lang)
    }
}

/// Is this node worth a fold chevron? Multi-line bracketed blocks (`{}`/`[]`/`(`/
/// `<`) plus indentation/structure nodes (Python `block`, YAML mappings, TOML
/// tables, HTML elements). Single-line or leaf nodes never fold.
fn is_foldable(node: &tree_sitter::Node, source: &[u8]) -> bool {
    if node.child_count() == 0 {
        return false;
    }
    let opens_with_bracket = source
        .get(node.start_byte())
        .is_some_and(|b| matches!(b, b'{' | b'[' | b'(' | b'<'));
    let kind = node.kind();
    opens_with_bracket
        || kind.contains("block")
        || kind.contains("body")
        || kind.contains("mapping")
        || kind.contains("sequence")
        || kind.contains("object")
        || kind.contains("array")
        || kind.contains("dictionary")
        || kind.contains("table")
        || kind == "element"
}

/// Foldable regions as `(start_line, end_line)` 0-based line indices, where
/// folding `start_line` hides `start_line+1 ..= end_line`. At most one region
/// per start line (the outermost / largest is kept). Empty when the language has
/// no installed grammar.
pub fn fold_regions(source: &str, lang: Lang) -> Vec<(usize, usize)> {
    let Some(grammar) = lang.grammar() else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let (sr, er) = (node.start_position().row, node.end_position().row);
        if er > sr && is_foldable(&node, bytes) {
            out.push((sr, er));
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    // One chevron per start line: sort by start asc, end desc; keep the first
    // (largest span) per start row.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    out.dedup_by_key(|r| r.0);
    out
}

/// Widen a zero-width range (a `MISSING` node) to cover one character so the UI
/// has something to underline: the char at the position, or the one before it at
/// end-of-source. Char-boundary safe; empty source stays empty.
fn widen_if_empty(source: &str, r: std::ops::Range<usize>) -> std::ops::Range<usize> {
    if r.start < r.end {
        return r;
    }
    let start = r.start.min(source.len());
    if let Some(c) = source[start..].chars().next() {
        return start..start + c.len_utf8();
    }
    let prev = source[..start]
        .chars()
        .next_back()
        .map_or(start, |c| start - c.len_utf8());
    prev..start
}

/// Byte ranges the parser could not make sense of: `ERROR` nodes (unparseable
/// source) plus `MISSING` nodes (a required token the parser inserted to
/// recover, e.g. an unclosed brace), sorted and merged. Empty when the source
/// parses cleanly or the language has no compiled-in grammar (`PlainText`, the
/// builtin line-highlighters, feature-trimmed builds). Zero-width `MISSING`
/// ranges are widened to one character so a squiggle can always be drawn.
///
/// ```
/// use kyde_syntax::{error_ranges, Lang};
/// assert!(error_ranges("{\"a\": 1}", Lang::Json).is_empty());
/// assert!(!error_ranges("{\"a\" 1}", Lang::Json).is_empty());
/// assert!(error_ranges("anything at all", Lang::PlainText).is_empty());
/// ```
pub fn error_ranges(source: &str, lang: Lang) -> Vec<std::ops::Range<usize>> {
    let Some(grammar) = lang.grammar() else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    // Fast path: a clean parse (the overwhelmingly common case) never walks.
    if !tree.root_node().has_error() {
        return Vec::new();
    }
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            // Children of an ERROR are partial re-parses of the broken region —
            // one range covering the whole node is the honest report.
            out.push(widen_if_empty(source, node.byte_range()));
            continue;
        }
        // `has_error` is true iff an ERROR/MISSING exists in the subtree, so
        // clean subtrees are pruned without visiting their children.
        if !node.has_error() {
            continue;
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for r in out {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

// ── sort object keys (JSON / JS / TS object literals) ─────────────

/// The sort key for an object entry: the key node's text with string quotes
/// stripped, lowercased for the primary comparison (raw text breaks ties, so
/// `"A"` and `"a"` order deterministically).
fn entry_key(node: tree_sitter::Node, src: &str) -> Option<(String, String)> {
    let key = match node.kind() {
        "pair" => node.child_by_field_name("key")?,
        // JS shorthand `{ foo, bar }` — the entry IS its key.
        "shorthand_property_identifier" => node,
        _ => return None,
    };
    let raw = src
        .get(key.byte_range())?
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    Some((raw.to_lowercase(), raw))
}

/// Rebuild `node`'s text with every descendant object's keys sorted.
/// Returns `None` when nothing under `node` changed (callers then reuse the
/// original slice — no allocation for already-sorted subtrees).
///
/// An object is only REORDERED when every named child is a `pair`/shorthand
/// entry — a spread (`...x`), method, comment, or parse error means order can
/// carry meaning, so that object keeps its order (children still recurse).
/// Entry texts move as-is; the separator texts between entries (comma +
/// newline + indent) stay in their original slots, so formatting survives.
fn sorted_text(node: tree_sitter::Node, src: &str) -> Option<String> {
    // Generic splice for non-objects (and non-reorderable objects): recurse into
    // children; if none changed, report unchanged.
    fn splice(node: tree_sitter::Node, src: &str) -> Option<String> {
        let mut out: Option<String> = None;
        let mut done = node.start_byte();
        for i in 0..node.child_count() {
            let Some(c) = node.child(i) else { continue };
            if let Some(new) = sorted_text(c, src) {
                let s = out.get_or_insert_with(|| String::with_capacity(src.len() / 8));
                *s += &src[done..c.start_byte()];
                *s += &new;
                done = c.end_byte();
            }
        }
        let mut s = out?;
        s += &src[done..node.end_byte()];
        Some(s)
    }

    if node.kind() != "object" {
        return splice(node, src);
    }
    let entries: Vec<tree_sitter::Node> = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .collect();
    let keys: Vec<Option<(String, String)>> = entries.iter().map(|e| entry_key(*e, src)).collect();
    if entries.is_empty() || keys.iter().any(Option::is_none) {
        return splice(node, src); // not a plain key/value object — never reorder
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b])); // stable; None already excluded
                                                   // Each entry's text, with ITS nested objects sorted first.
    let texts: Vec<String> = entries
        .iter()
        .map(|e| sorted_text(*e, src).unwrap_or_else(|| src[e.byte_range()].to_string()))
        .collect();
    let unchanged = order.iter().enumerate().all(|(slot, &i)| slot == i)
        && texts
            .iter()
            .zip(&entries)
            .all(|(t, e)| t == &src[e.byte_range()]);
    if unchanged {
        return None;
    }
    // Rebuild: original prefix, then per slot the sorted entry followed by the
    // ORIGINAL separator that sat after that slot (comma/newline/indent layout).
    let mut s = String::with_capacity(node.end_byte() - node.start_byte() + 16);
    s += &src[node.start_byte()..entries[0].start_byte()];
    for (slot, &i) in order.iter().enumerate() {
        s += &texts[i];
        let sep_from = entries[slot].end_byte();
        let sep_to = entries
            .get(slot + 1)
            .map_or(node.end_byte(), tree_sitter::Node::start_byte);
        s += &src[sep_from..sep_to];
    }
    Some(s)
}

/// Does `node`'s subtree contain any object literal?
fn contains_object(node: tree_sitter::Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "object" {
            return true;
        }
        for i in 0..n.child_count() {
            if let Some(c) = n.child(i) {
                stack.push(c);
            }
        }
    }
    false
}

/// Sort object keys around `sel`, recursively (nested objects sort too; arrays
/// keep their element order but object elements inside them still sort).
///
/// Target: a collapsed `sel` (a caret) sorts the innermost object containing
/// it. A ranged `sel` first trims surrounding whitespace (so selecting a block
/// including its indent targets the block, not the enclosing object), then
/// sorts the smallest node covering it — when that node isn't itself an object
/// (a selection across sibling objects in an array, say), EVERY object inside
/// it sorts while the node's own order stays put. Formatting is preserved:
/// entry texts move verbatim and the comma/indent layout stays put. Objects
/// containing anything other than plain key/value entries (spreads, methods,
/// comments, parse errors) keep their order. Returns the rewritten byte range
/// plus its new text — equal to the original when already sorted — or `None`
/// when the language isn't JSON/JS/TS or there is no object at/under `sel`.
///
/// ```
/// use kyde_syntax::{sort_object_keys, Lang};
/// let (r, s) = sort_object_keys("{\"b\": 1, \"a\": 2}", Lang::Json, 3..3).unwrap();
/// assert_eq!(r, 0..16);
/// assert_eq!(s, "{\"a\": 2, \"b\": 1}");
/// assert!(sort_object_keys("[1, 2]", Lang::Json, 1..1).is_none());
/// ```
pub fn sort_object_keys(
    source: &str,
    lang: Lang,
    sel: std::ops::Range<usize>,
) -> Option<(std::ops::Range<usize>, String)> {
    // Only grammars whose object literals we understand (`object` + `pair`).
    if !matches!(lang, Lang::Json | Lang::Js | Lang::Ts | Lang::Tsx) {
        return None;
    }
    let grammar = lang.grammar()?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).ok()?;
    let tree = parser.parse(source, None)?;
    // Trim the selection to its non-whitespace core; an all-whitespace or
    // collapsed selection degenerates to a caret.
    let (mut a, mut b) = (sel.start.min(source.len()), sel.end.min(source.len()));
    if let Some(i) = source[a..b].find(|c: char| !c.is_whitespace()) {
        a += i;
        b = a
            + source[a..b]
                .rfind(|c: char| !c.is_whitespace())
                .unwrap_or(0)
            + 1;
    } else {
        b = a;
    }
    let mut node = tree.root_node().descendant_for_byte_range(a, b)?;
    // A ranged selection whose covering node holds objects sorts them all in
    // place; otherwise (caret, or a selection inside a leaf) fall back to the
    // innermost ENCLOSING object.
    if a == b || !contains_object(node) {
        node = loop {
            if node.kind() == "object" {
                break node;
            }
            node = node.parent()?;
        };
    }
    let range = node.byte_range();
    let text = sorted_text(node, source).unwrap_or_else(|| source[range.clone()].to_string());
    Some((range, text))
}

/// Iterate `(byte_start, line)` over `source`, tracking byte offsets including '\n'.
fn lines_with_offsets(source: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut off = 0usize;
    source.split('\n').map(move |line| {
        let start = off;
        off += line.len() + 1; // +1 for the '\n' separator
        (start, line)
    })
}

/// .gitignore: comment lines (`# …`) gray; everything else default.
fn highlight_gitignore(source: &str) -> Vec<Span> {
    // Resolve the theme once, not per line (`theme::get()` is an RwLock read + Theme copy,
    // and this runs on every keystroke) — same hoist as `highlight`/`highlight_env`.
    let t = theme::get();
    let mut spans = Vec::new();
    for (start, line) in lines_with_offsets(source) {
        if line.trim_start().starts_with('#') {
            spans.push(Span {
                start,
                end: start + line.len(),
                color: t.syn_comment,
            });
        }
    }
    spans
}

/// .env: `# …` comments gray; `KEY=value` → key as field, `=` operator, value as string.
fn highlight_env(source: &str) -> Vec<Span> {
    let t = theme::get();
    let mut spans = Vec::new();
    for (start, line) in lines_with_offsets(source) {
        if line.trim_start().starts_with('#') {
            spans.push(Span {
                start,
                end: start + line.len(),
                color: t.syn_comment,
            });
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        if eq > 0 {
            spans.push(Span {
                start,
                end: start + eq,
                color: t.syn_field,
            });
        }
        spans.push(Span {
            start: start + eq,
            end: start + eq + 1,
            color: t.syn_operator,
        });
        let val_start = start + eq + 1;
        let val_end = start + line.len();
        if val_end > val_start {
            spans.push(Span {
                start: val_start,
                end: val_end,
                color: t.syn_string,
            });
        }
    }
    spans
}

/// Highlight `source` for the given language into ordered, non-overlapping spans.
/// Gaps between spans render in the default foreground.
pub fn highlight(source: &str, lang: Lang) -> Vec<Span> {
    match lang {
        Lang::Env => return highlight_env(source),
        Lang::Gitignore => return highlight_gitignore(source),
        _ => {}
    }
    let Some(config) = lang.config() else {
        return Vec::new();
    };
    let mut hl = Highlighter::new();
    let mut spans = Vec::new();
    let Ok(events) = hl.highlight(&config, source.as_bytes(), None, |_| None) else {
        return spans;
    };

    // Resolve the theme once, not per token: `theme::get()` takes an `RwLock` read and
    // copies the whole `Theme`, and this loop runs per source token on every keystroke.
    let t = theme::get();
    let mut stack: Vec<usize> = Vec::new();
    for ev in events.flatten() {
        match ev {
            HighlightEvent::HighlightStart(h) => stack.push(h.0),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let color = match stack.last() {
                    Some(&idx) => capture_color(&t, CAPTURES[idx]),
                    None => t.text,
                };
                spans.push(Span { start, end, color });
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TS/TSX inherits its base highlighting from the JS query; if that prepend ever
    /// regresses, every token collapses to the default color (one distinct color).
    #[test]
    fn typescript_highlights_with_real_colors() {
        let src = "function hello() {\n  const msg = \"hi\";\n  return 42;\n}\n";
        for lang in [Lang::Tsx, Lang::Ts] {
            let distinct: std::collections::HashSet<u32> = highlight(src, lang)
                .iter()
                .map(|s| {
                    ((s.color.r * 255.0) as u32) << 16
                        | ((s.color.g * 255.0) as u32) << 8
                        | (s.color.b * 255.0) as u32
                })
                .collect();
            assert!(
                distinct.len() >= 3,
                "{lang:?} highlighting collapsed to {} color(s) — JS base query missing?",
                distinct.len()
            );
        }
    }

    #[test]
    fn highlights_rust_keyword() {
        let spans = highlight("fn main() {}", Lang::Rust);
        assert!(!spans.is_empty());
        // first span should cover "fn" and be the keyword color
        assert_eq!(spans[0].start, 0);
    }

    #[test]
    fn plain_text_has_no_spans() {
        assert!(highlight("hello", Lang::PlainText).is_empty());
    }

    #[test]
    fn detects_filename_types() {
        use std::path::Path;
        assert_eq!(Lang::from_path(Path::new(".gitignore")), Lang::Gitignore);
        assert_eq!(Lang::from_path(Path::new(".env")), Lang::Env);
        assert_eq!(Lang::from_path(Path::new(".env.local")), Lang::Env);
        assert_eq!(Lang::from_path(Path::new("a/b/styles.scss")), Lang::Scss);
        assert_eq!(Lang::from_path(Path::new("ci/config.yml")), Lang::Yaml);
        assert_eq!(
            Lang::from_path(Path::new("docker-compose.yaml")),
            Lang::Yaml
        );
        assert_eq!(Lang::from_path(Path::new("README.md")), Lang::Markdown);
        assert_eq!(Lang::from_path(Path::new("deploy.sh")), Lang::Bash);
        assert_eq!(Lang::from_path(Path::new("analysis.R")), Lang::R);
        assert_eq!(Lang::from_path(Path::new("model.r")), Lang::R);
        assert_eq!(Lang::from_path(Path::new("paper.tex")), Lang::Latex);
        assert_eq!(Lang::from_path(Path::new("x.unknown")), Lang::PlainText);
    }

    #[test]
    fn every_lang_with_a_pack_actually_highlights() {
        // Each installable language must produce spans (grammar wired correctly).
        let cases = [
            ("{\"a\":1}", Lang::Json),
            ("const x: number = 1;", Lang::Ts),
            ("const x = <div/>;", Lang::Tsx),
            ("# Title\n\ntext", Lang::Markdown),
            ("echo $HOME # hi", Lang::Bash),
            ("a { color: red; }", Lang::Css),
            ("$c: red;\na { color: $c; }", Lang::Scss),
            ("name: kyde\nversion: 1\n", Lang::Yaml),
            ("[package]\nname = \"x\"\n", Lang::Toml),
            ("def f(x):\n    return x\n", Lang::Python),
            ("<div class=\"a\">hi</div>", Lang::Html),
            ("package main\nfunc main() {}\n", Lang::Go),
            ("x <- 1  # comment\nf <- function(y) y + 1\n", Lang::R),
            ("\\section{Hi}  % comment\n\\ref{fig:1}\n", Lang::Latex),
        ];
        for (src, lang) in cases {
            assert!(!highlight(src, lang).is_empty(), "no spans for {lang:?}");
        }
    }

    #[test]
    fn folds_json_object_and_array() {
        // {                ← line 0, foldable through line 4
        //   "a": 1,        ← line 1
        //   "b": [         ← line 2, foldable through line 3 (the array)
        //     2
        //   ]
        // }
        let src = "{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}";
        let regions = fold_regions(src, Lang::Json);
        assert!(
            regions.iter().any(|&(s, e)| s == 0 && e == 5),
            "top object: {regions:?}"
        );
        assert!(
            regions.iter().any(|&(s, _)| s == 2),
            "inner array start: {regions:?}"
        );
        // single-line / leaf nodes never fold
        assert!(fold_regions("{\"a\":1}", Lang::Json)
            .iter()
            .all(|&(s, e)| e > s));
        // no grammar → no folds
        assert!(fold_regions("a\nb\n", Lang::PlainText).is_empty());
    }

    #[test]
    fn error_ranges_flags_invalid_source() {
        // Extra comma is an ERROR node in the JSON grammar.
        let bad = "{\"a\": 1,, \"b\": 2}";
        let ranges = error_ranges(bad, Lang::Json);
        assert!(!ranges.is_empty(), "invalid JSON produced no error ranges");
        for r in &ranges {
            assert!(r.start < r.end, "empty range {r:?}");
            assert!(r.end <= bad.len(), "range {r:?} out of bounds");
            assert!(
                bad.is_char_boundary(r.start) && bad.is_char_boundary(r.end),
                "range {r:?} splits a char"
            );
        }
        // Clean parses stay empty.
        assert!(error_ranges("{\"a\": 1}", Lang::Json).is_empty());
        assert!(error_ranges("fn main() {}", Lang::Rust).is_empty());
        // No grammar → no ranges, ever.
        assert!(error_ranges("{{{{", Lang::PlainText).is_empty());
        assert!(error_ranges("KEY=", Lang::Env).is_empty());
    }

    #[test]
    fn error_ranges_widens_missing_at_eof() {
        // Unclosed object → zero-width MISSING `}` at end-of-source, widened to
        // cover the last character so the UI can draw a squiggle.
        let src = "{\"a\": 1";
        let ranges = error_ranges(src, Lang::Json);
        assert!(!ranges.is_empty(), "unclosed JSON produced no error ranges");
        assert!(ranges.iter().all(|r| r.start < r.end && r.end <= src.len()));
        // Ranges are sorted and non-overlapping after the merge pass.
        for w in ranges.windows(2) {
            assert!(w[0].end <= w[1].start, "unmerged overlap: {ranges:?}");
        }
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `error_ranges` runs on every keystroke (alongside `highlight` +
    /// `fold_regions`) when a language's error highlighting is opted in, so it
    /// must stay parse-speed. Two shapes: a big file with ONE error at the end
    /// (the walk must prune clean subtrees, not visit ~4000 lines of nodes) and
    /// an error-DENSE file (every statement broken — the walk visits everything).
    #[test]
    fn perf_error_ranges_large_files_stay_fast() {
        let unit = "fn f(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\n";
        let mostly_clean = format!("{}fn broken(", unit.repeat(1000));
        let error_dense = "let x = ;\n".repeat(4000);
        let start = std::time::Instant::now();
        let sparse = error_ranges(&mostly_clean, Lang::Rust);
        let dense = error_ranges(&error_dense, Lang::Rust);
        let elapsed = start.elapsed();
        assert!(!sparse.is_empty() && !dense.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "error_ranges on ~4000-line files took {elapsed:?} (budget 2s) — perf regression?"
        );
    }

    #[test]
    fn sort_keys_nested_json_preserves_formatting() {
        let src = "{\n  \"b\": {\n    \"z\": 1,\n    \"a\": 2\n  },\n  \"a\": [\n    3,\n    { \"y\": 1, \"x\": 2 }\n  ]\n}";
        let (range, out) = sort_object_keys(src, Lang::Json, 5..5).unwrap();
        assert_eq!(
            range,
            0..src.len(),
            "innermost object at caret 5 is the root"
        );
        let want = "{\n  \"a\": [\n    3,\n    { \"x\": 2, \"y\": 1 }\n  ],\n  \"b\": {\n    \"a\": 2,\n    \"z\": 1\n  }\n}";
        assert_eq!(
            out, want,
            "keys sort recursively, arrays keep element order, layout survives"
        );
    }

    #[test]
    fn sort_keys_innermost_object_only() {
        let src = "{\"b\": 1, \"a\": {\"d\": 1, \"c\": 2}}";
        // Caret inside the nested object → only that object's range.
        let inner_start = src.find("{\"d\"").unwrap();
        let (range, out) =
            sort_object_keys(src, Lang::Json, inner_start + 2..inner_start + 2).unwrap();
        assert_eq!(range, inner_start..src.len() - 1);
        assert_eq!(out, "{\"c\": 2, \"d\": 1}");
    }

    #[test]
    fn sort_keys_selection_targets_the_selected_object() {
        // The user's release-please case: an object inside an array, selected
        // WITH its leading indent — the selection must sort THAT object, not
        // the enclosing one (the whitespace trim), and nested objects inside a
        // ranged selection sort too.
        let src = "{\n  \"outer\": [\n    {\n      \"type\": \"toml\",\n      \"path\": \"x\",\n      \"jsonpath\": \"$.v\"\n    }\n  ]\n}";
        let sel_start = src.find("    {").unwrap(); // line start incl. indent
        let sel_end = src.find("\n  ]").unwrap();
        let (range, out) = sort_object_keys(src, Lang::Json, sel_start..sel_end).unwrap();
        assert_eq!(range, src.find("{\n      ").unwrap()..sel_end);
        assert_eq!(
            out,
            "{\n      \"jsonpath\": \"$.v\",\n      \"path\": \"x\",\n      \"type\": \"toml\"\n    }"
        );
        // A selection spanning SIBLING objects in an array sorts each of them
        // (array element order untouched).
        let src = "[\n  {\"b\": 1, \"a\": 2},\n  {\"d\": 3, \"c\": 4}\n]";
        let (range, out) = sort_object_keys(src, Lang::Json, 2..src.len() - 1).unwrap();
        assert_eq!(range, 0..src.len());
        assert_eq!(out, "[\n  {\"a\": 2, \"b\": 1},\n  {\"c\": 4, \"d\": 3}\n]");
    }

    #[test]
    fn sort_keys_js_shorthand_and_spread() {
        // Shorthand entries sort by their own name.
        let (_, out) = sort_object_keys("const o = { b, a, c: 1 };", Lang::Js, 14..14).unwrap();
        assert_eq!(out, "{ a, b, c: 1 }");
        // A spread means order can matter — never reorder that object…
        let src = "const o = { z: 1, ...rest, a: 2 };";
        let (_, out) = sort_object_keys(src, Lang::Js, 14..14).unwrap();
        assert_eq!(out, "{ z: 1, ...rest, a: 2 }");
        // …but an object nested under it still sorts.
        let src = "const o = { ...rest, v: { b: 1, a: 2 } };";
        let (_, out) = sort_object_keys(src, Lang::Js, 13..13).unwrap();
        assert_eq!(out, "{ ...rest, v: { a: 2, b: 1 } }");
    }

    #[test]
    fn sort_keys_none_cases_and_already_sorted() {
        // No object at caret (array root) / unsupported lang.
        assert!(sort_object_keys("[1, 2, 3]", Lang::Json, 2..2).is_none());
        assert!(sort_object_keys("{\"b\":1,\"a\":2}", Lang::PlainText, 2..2).is_none());
        assert!(sort_object_keys("b: 1\na: 2\n", Lang::Yaml, 2..2).is_none());
        // Already sorted → text comes back identical (caller no-ops).
        let src = "{\"a\": 1, \"b\": 2}";
        let (range, out) = sort_object_keys(src, Lang::Json, 3..3).unwrap();
        assert_eq!(&src[range], out);
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn sort_keys_typescript_object() {
        let src = "const x: T = { beta: 2, alpha: 1 };";
        let (_, out) = sort_object_keys(src, Lang::Ts, 16..16).unwrap();
        assert_eq!(out, "{ alpha: 1, beta: 2 }");
    }

    #[test]
    fn builtin_env_and_gitignore() {
        let env = highlight("# comment\nKEY=value\n", Lang::Env);
        assert!(env.len() >= 3, "env: comment + key + op + value");
        let ignore = highlight("# rule\nnode_modules\n", Lang::Gitignore);
        assert_eq!(ignore.len(), 1, "only the comment line is colored");
    }

    #[test]
    fn plaintext_has_no_pack_but_known_langs_do() {
        assert!(Lang::PlainText.pack().is_none());
        assert_eq!(Lang::Ts.pack().unwrap().id, "typescript");
        assert_eq!(Lang::Tsx.pack().unwrap().id, "typescript");
        assert_eq!(Lang::Scss.pack().unwrap().id, "scss");
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `highlight` + `fold_regions` both run on EVERY keystroke (the editor
    /// re-highlights and recomputes folds per edit), so a regression here is felt
    /// directly as typing lag. Budget is deliberately loose (catches algorithmic
    /// blowups / accidental re-parse loops, not CI jitter); on a dev machine this
    /// runs in tens of ms.
    #[test]
    fn perf_highlight_and_fold_large_file_stays_fast() {
        let unit = "fn f(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\n";
        let src = unit.repeat(1000); // ~4000 lines of real Rust
        let start = std::time::Instant::now();
        let spans = highlight(&src, Lang::Rust);
        let folds = fold_regions(&src, Lang::Rust);
        let elapsed = start.elapsed();
        assert!(!spans.is_empty() && !folds.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "highlight+fold of ~4000 lines took {elapsed:?} (budget 2s) — perf regression?"
        );
    }

    /// Big-file guard modeled on a real `package-lock.json` (~15k lines): deeply
    /// nested objects, many string/version values. This is the file type that made
    /// the editor feel slow, so it earns its own guard. `highlight` runs once per
    /// content change (now cached, not per-frame — see editor.rs), and `fold_regions`
    /// alongside it; both must stay well clear of typing-lag territory.
    #[cfg(feature = "json")]
    #[test]
    fn perf_highlight_large_package_lock_json_stays_fast() {
        // One dependency entry, repeated — mirrors npm lockfile shape & nesting.
        let entry = r#"    "node_modules/some-package-name": {
      "version": "1.2.3",
      "resolved": "https://registry.npmjs.org/some-package-name/-/some-package-name-1.2.3.tgz",
      "integrity": "sha512-AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIIIJJJJKKKKLLLLMMMMNNNNOOOOPPPP==",
      "dev": true,
      "dependencies": {
        "nested-dep": "^4.5.6",
        "another-dep": "~7.8.9"
      }
    },
"#;
        // ~1500 entries × ~10 lines ≈ 15k lines, a large-but-real lockfile.
        let src = format!("{{\n  \"packages\": {{\n{}  }}\n}}\n", entry.repeat(1500));
        let start = std::time::Instant::now();
        let spans = highlight(&src, Lang::Json);
        let folds = fold_regions(&src, Lang::Json);
        let elapsed = start.elapsed();
        assert!(!spans.is_empty() && !folds.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "highlight+fold of ~15k-line package-lock.json took {elapsed:?} (budget 3s) — perf regression?"
        );
    }
}
