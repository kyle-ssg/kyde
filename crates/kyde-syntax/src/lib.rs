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

// ── import links (cmd+click to open — issue #26) ───────────────────

/// What kind of import an [`ImportLink`] came from — drives resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// A module specifier: a quoted path in TS/JS (`import x from "./y"`) or a
    /// dotted module path in Python (`import a.b`, `from .rel import c`).
    Specifier,
    /// Rust `mod name;` (no body) — the module lives in a sibling file.
    RustMod,
    /// A Rust `use` path — resolved from the crate's `src/` root (best effort).
    RustUse,
}

/// One clickable import reference in a buffer.
#[derive(Debug, Clone)]
pub struct ImportLink {
    /// Byte range to underline (the specifier/path text, quotes excluded).
    pub range: std::ops::Range<usize>,
    /// The raw specifier text (used by [`resolve_import`]).
    pub target: String,
    /// Which import form produced it.
    pub kind: ImportKind,
}

/// Extract the import references of `source` for cmd+click navigation.
/// Supported: Rust (`mod x;` without a body, `use` paths), TypeScript/TSX/
/// JavaScript (`import`/`export … from "x"`, `require("x")`, dynamic
/// `import("x")`), and Python (`import a.b`, `from .rel import c`). Other
/// languages (or a feature-trimmed build) return no links.
///
/// ```
/// use kyde_syntax::{import_links, Lang};
/// let links = import_links("import { a } from './x';\n", Lang::Ts);
/// assert_eq!(links.len(), 1);
/// assert_eq!(links[0].target, "./x");
/// assert!(import_links("hello\n", Lang::PlainText).is_empty());
/// ```
pub fn import_links(source: &str, lang: Lang) -> Vec<ImportLink> {
    imports_with_bindings(source, lang)
        .into_iter()
        .map(|(l, _)| l)
        .collect()
}

/// The local names each import binds, paired with that import's [`ImportLink`]
/// — so ⌘-clicking a USE of an imported symbol can jump through to the file it
/// came from. Rust `use` names/aliases/lists, TS/JS default + named (incl.
/// `as`) + namespace imports, Python module first-segments and `from`-names.
/// Wildcards bind nothing knowable and are skipped.
#[must_use]
pub fn import_bindings(source: &str, lang: Lang) -> Vec<(String, ImportLink)> {
    imports_with_bindings(source, lang)
        .into_iter()
        .flat_map(|(l, names)| names.into_iter().map(move |n| (n, l.clone())))
        .collect()
}

/// Shared walk behind [`import_links`] / [`import_bindings`]: every import in
/// `source` as `(link, names it binds locally)`, sorted by position.
fn imports_with_bindings(source: &str, lang: Lang) -> Vec<(ImportLink, Vec<String>)> {
    if !matches!(
        lang,
        Lang::Rust | Lang::Ts | Lang::Tsx | Lang::Js | Lang::Python
    ) {
        return Vec::new();
    }
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
    let mut out: Vec<(ImportLink, Vec<String>)> = Vec::new();
    let text = |n: tree_sitter::Node| source.get(n.byte_range()).unwrap_or("").to_string();
    let link = |n: tree_sitter::Node, kind: ImportKind| -> Option<ImportLink> {
        let target = source.get(n.byte_range()).unwrap_or("").to_string();
        (!target.is_empty()).then_some(ImportLink {
            range: n.byte_range(),
            target,
            kind,
        })
    };
    // The names a Rust use-tree binds: plain/scoped idents bind their last
    // segment, `as` clauses their alias, `{…}` lists each entry (recursively);
    // wildcards bind nothing knowable.
    fn rust_use_names(n: tree_sitter::Node, src: &str, out: &mut Vec<String>) {
        let txt = |m: tree_sitter::Node| src.get(m.byte_range()).unwrap_or("").to_string();
        match n.kind() {
            "identifier" => out.push(txt(n)),
            "scoped_identifier" => {
                if let Some(name) = n.child_by_field_name("name") {
                    out.push(txt(name));
                }
            }
            "use_as_clause" => {
                if let Some(a) = n.child_by_field_name("alias") {
                    out.push(txt(a));
                }
            }
            "scoped_use_list" | "use_list" => {
                let list = n.child_by_field_name("list").unwrap_or(n);
                for i in 0..list.named_child_count() {
                    if let Some(c) = list.named_child(i) {
                        rust_use_names(c, src, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match (lang, node.kind()) {
            // Rust: `mod name;` with no inline body → sibling file module. Binds
            // the module name (`widgets::x` paths then resolve via the mod file).
            (Lang::Rust, "mod_item") => {
                if node.child_by_field_name("body").is_none() {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Some(l) = link(name, ImportKind::RustMod) {
                            let names = vec![text(name)];
                            out.push((l, names));
                        }
                    }
                }
            }
            // Rust: the `use` path — for `use a::b::{c, d}` link the `a::b` prefix.
            (Lang::Rust, "use_declaration") => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    let path = match arg.kind() {
                        "scoped_use_list" | "use_as_clause" => arg.child_by_field_name("path"),
                        // `use path::*` — the wildcard wraps its path as the first child.
                        "use_wildcard" => arg.named_child(0),
                        "scoped_identifier" | "identifier" | "crate" | "super" | "self" => {
                            Some(arg)
                        }
                        _ => None,
                    };
                    if let Some(p) = path {
                        if let Some(l) = link(p, ImportKind::RustUse) {
                            let mut names = Vec::new();
                            rust_use_names(arg, source, &mut names);
                            out.push((l, names));
                        }
                    }
                }
            }
            // TS/TSX/JS: `import … from "x"` / `export … from "x"`. Only the
            // import form binds local names (default / named / namespace).
            (Lang::Ts | Lang::Tsx | Lang::Js, "import_statement" | "export_statement") => {
                if let Some(s) = node.child_by_field_name("source") {
                    if let Some(l) = link(string_body(s), ImportKind::Specifier) {
                        let mut names = Vec::new();
                        if node.kind() == "import_statement" {
                            let mut st = vec![node];
                            while let Some(n) = st.pop() {
                                match n.kind() {
                                    // Default import + namespace ident both land here.
                                    "identifier" => names.push(text(n)),
                                    "import_specifier" => {
                                        if let Some(b) = n
                                            .child_by_field_name("alias")
                                            .or_else(|| n.child_by_field_name("name"))
                                        {
                                            names.push(text(b));
                                        }
                                        continue;
                                    }
                                    "string" => continue, // the source — not a binding
                                    _ => {}
                                }
                                for i in 0..n.named_child_count() {
                                    if let Some(c) = n.named_child(i) {
                                        st.push(c);
                                    }
                                }
                            }
                        }
                        out.push((l, names));
                    }
                }
            }
            // TS/TSX/JS: `require("x")` and dynamic `import("x")`. The binding
            // (if any) is an ordinary variable declarator — a local definition —
            // so the import itself binds nothing here.
            (Lang::Ts | Lang::Tsx | Lang::Js, "call_expression") => {
                let is_import_call = node.child_by_field_name("function").is_some_and(|f| {
                    f.kind() == "import" || (f.kind() == "identifier" && text(f) == "require")
                });
                if is_import_call {
                    if let Some(s) = node
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .filter(|a| a.kind() == "string")
                    {
                        if let Some(l) = link(string_body(s), ImportKind::Specifier) {
                            out.push((l, Vec::new()));
                        }
                    }
                }
            }
            // Python: `import a.b, c` — each dotted/aliased name. `import a.b`
            // binds `a` (the first segment); an alias binds the alias.
            (Lang::Python, "import_statement") => {
                for i in 0..node.named_child_count() {
                    let Some(c) = node.named_child(i) else {
                        continue;
                    };
                    let (name, bound) = match c.kind() {
                        "dotted_name" => {
                            let first = c.named_child(0).map(text);
                            (Some(c), first)
                        }
                        "aliased_import" => (
                            c.child_by_field_name("name"),
                            c.child_by_field_name("alias").map(text),
                        ),
                        _ => (None, None),
                    };
                    if let Some(n) = name {
                        if let Some(l) = link(n, ImportKind::Specifier) {
                            out.push((l, bound.into_iter().collect()));
                        }
                    }
                }
            }
            // Python: `from a.b import c as d, e` — binds d + e against the module.
            (Lang::Python, "import_from_statement") => {
                if let Some(m) = node.child_by_field_name("module_name") {
                    if let Some(l) = link(m, ImportKind::Specifier) {
                        let mut names = Vec::new();
                        let mut w = node.walk();
                        for c in node.children_by_field_name("name", &mut w) {
                            match c.kind() {
                                "dotted_name" => {
                                    if let Some(f) = c.named_child(0) {
                                        names.push(text(f));
                                    }
                                }
                                "aliased_import" => {
                                    if let Some(a) = c.child_by_field_name("alias") {
                                        names.push(text(a));
                                    }
                                }
                                _ => {}
                            }
                        }
                        out.push((l, names));
                    }
                }
            }
            _ => {}
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    out.sort_by_key(|(l, _)| l.range.start);
    out
}

/// Definition sites in `source` as `(name, byte range of the name)`, sorted by
/// position — the go-to-definition index behind ⌘-clicking a variable/type.
/// Covers the declaration forms of the supported languages (Rust items +
/// `let`/params, TS/JS declarations + declarators/params/methods, Python
/// `def`/`class`/assignments/params); other languages return no sites.
///
/// ```
/// use kyde_syntax::{definition_sites, Lang};
/// let sites = definition_sites("const foo = 1;\n", Lang::Ts);
/// assert_eq!(sites.len(), 1);
/// assert_eq!(sites[0].0, "foo");
/// ```
#[must_use]
pub fn definition_sites(source: &str, lang: Lang) -> Vec<(String, std::ops::Range<usize>)> {
    if !matches!(
        lang,
        Lang::Rust | Lang::Ts | Lang::Tsx | Lang::Js | Lang::Python
    ) {
        return Vec::new();
    }
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
    let mut out: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    let push = |out: &mut Vec<(String, std::ops::Range<usize>)>, n: tree_sitter::Node| {
        if let Some(t) = source.get(n.byte_range()) {
            if !t.is_empty() {
                out.push((t.to_string(), n.byte_range()));
            }
        }
    };
    // Collect the bound identifiers inside a (possibly destructuring) pattern.
    // Property KEYS are `property_identifier`/`field_identifier` nodes, not
    // `identifier`, so walking for identifiers never picks up map keys.
    let bind_pattern = |out: &mut Vec<(String, std::ops::Range<usize>)>, p: tree_sitter::Node| {
        let mut st = vec![p];
        while let Some(n) = st.pop() {
            match n.kind() {
                "identifier" | "shorthand_property_identifier_pattern" => push(out, n),
                _ => {
                    for i in 0..n.named_child_count() {
                        if let Some(c) = n.named_child(i) {
                            st.push(c);
                        }
                    }
                }
            }
        }
    };
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match (lang, node.kind()) {
            // Rust items — the `name` field is the definition site.
            (
                Lang::Rust,
                "function_item" | "struct_item" | "enum_item" | "union_item" | "trait_item"
                | "type_item" | "const_item" | "static_item" | "mod_item" | "macro_definition",
            )
            | (
                Lang::Ts | Lang::Tsx | Lang::Js,
                "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "method_definition",
            )
            | (Lang::Python, "function_definition" | "class_definition") => {
                if let Some(name) = node.child_by_field_name("name") {
                    push(&mut out, name);
                }
            }
            // Rust `let` bindings + parameters (incl. destructuring patterns).
            (Lang::Rust, "let_declaration" | "parameter" | "for_expression") => {
                if let Some(p) = node.child_by_field_name("pattern") {
                    bind_pattern(&mut out, p);
                }
            }
            (Lang::Rust, "closure_parameters") => bind_pattern(&mut out, node),
            // TS/JS `const`/`let`/`var` declarators + parameters.
            (Lang::Ts | Lang::Tsx | Lang::Js, "variable_declarator") => {
                if let Some(n) = node.child_by_field_name("name") {
                    bind_pattern(&mut out, n);
                }
            }
            (Lang::Ts | Lang::Tsx | Lang::Js, "required_parameter" | "optional_parameter") => {
                if let Some(p) = node.child_by_field_name("pattern") {
                    bind_pattern(&mut out, p);
                }
            }
            // `x => …` — the bare arrow parameter is a plain identifier.
            (Lang::Ts | Lang::Tsx | Lang::Js, "arrow_function") => {
                if let Some(p) = node
                    .child_by_field_name("parameter")
                    .filter(|p| p.kind() == "identifier")
                {
                    push(&mut out, p);
                }
            }
            // Python assignments, parameters, and `for` targets.
            (Lang::Python, "assignment") => {
                if let Some(l) = node.child_by_field_name("left") {
                    bind_pattern(&mut out, l);
                }
            }
            (Lang::Python, "parameters") => bind_pattern(&mut out, node),
            (Lang::Python, "for_statement") => {
                if let Some(l) = node.child_by_field_name("left") {
                    bind_pattern(&mut out, l);
                }
            }
            _ => {}
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                stack.push(c);
            }
        }
    }
    out.sort_by_key(|(_, r)| r.start);
    out
}

/// The unquoted body of a TS/JS `string` node (its `string_fragment` child), so
/// the link range excludes the quote characters. An empty string (`""`) has no
/// fragment — the node itself comes back and produces an empty, dropped target.
fn string_body(s: tree_sitter::Node) -> tree_sitter::Node {
    for i in 0..s.named_child_count() {
        if let Some(c) = s.named_child(i) {
            if c.kind() == "string_fragment" {
                return c;
            }
        }
    }
    s
}

/// Resolve an [`ImportLink`] to a project file, best effort. `current` is the
/// file being edited and `files` the project's (repo-relative) file list; both
/// use the same relative paths the Browse tree serves. Returns `None` for
/// external modules (npm packages, crates.io deps, python stdlib) and anything
/// that doesn't land on a listed file.
#[must_use]
pub fn resolve_import(
    link: &ImportLink,
    lang: Lang,
    current: &Path,
    files: &[std::path::PathBuf],
) -> Option<std::path::PathBuf> {
    let exists = |p: &std::path::PathBuf| files.iter().any(|f| f == p);
    let dir = current.parent().unwrap_or_else(|| Path::new(""));
    // Join + normalize `.`/`..` without touching the filesystem.
    let norm = |base: &Path, rel: &str| -> std::path::PathBuf {
        let mut out: Vec<std::ffi::OsString> = base
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        for seg in rel.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    out.pop();
                }
                s => out.push(s.into()),
            }
        }
        out.iter().collect()
    };
    let first = |cands: Vec<std::path::PathBuf>| cands.into_iter().find(exists);
    match (lang, link.kind) {
        // TS/JS: relative specifiers only (a bare specifier is a package).
        (Lang::Ts | Lang::Tsx | Lang::Js, ImportKind::Specifier) => {
            if !link.target.starts_with('.') {
                return None;
            }
            let base = norm(dir, &link.target);
            let mut cands = vec![base.clone()];
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "d.ts"] {
                cands.push(std::path::PathBuf::from(format!(
                    "{}.{ext}",
                    base.to_string_lossy()
                )));
            }
            for idx in ["index.ts", "index.tsx", "index.js", "index.jsx"] {
                cands.push(base.join(idx));
            }
            first(cands)
        }
        // Python: leading dots walk up from the current file's package.
        (Lang::Python, ImportKind::Specifier) => {
            let dots = link.target.chars().take_while(|&c| c == '.').count();
            let rest = link.target[dots..].replace('.', "/");
            let mut cands = Vec::new();
            if dots > 0 {
                let mut base = dir.to_path_buf();
                for _ in 1..dots {
                    base = base.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
                }
                let p = if rest.is_empty() {
                    base
                } else {
                    norm(&base, &rest)
                };
                cands.push(std::path::PathBuf::from(format!(
                    "{}.py",
                    p.to_string_lossy()
                )));
                cands.push(p.join("__init__.py"));
            } else {
                // Absolute module: try the project root, then the current dir.
                for base in [Path::new(""), dir] {
                    let p = norm(base, &rest);
                    cands.push(std::path::PathBuf::from(format!(
                        "{}.py",
                        p.to_string_lossy()
                    )));
                    cands.push(p.join("__init__.py"));
                }
            }
            first(cands)
        }
        // Rust `mod name;`: a sibling `name.rs` or `name/mod.rs`.
        (Lang::Rust, ImportKind::RustMod) => first(vec![
            dir.join(format!("{}.rs", link.target)),
            dir.join(&link.target).join("mod.rs"),
        ]),
        // Rust `use` path: crate:: from src/, super:: from the parent module,
        // self:: from here; progressively shorter suffixes since the last
        // segments are usually items, not modules.
        (Lang::Rust, ImportKind::RustUse) => {
            let mut segs: Vec<&str> = link.target.split("::").collect();
            let base = match segs.first().copied() {
                Some("crate") => {
                    segs.remove(0);
                    std::path::PathBuf::from("src")
                }
                Some("super") => {
                    segs.remove(0);
                    dir.parent().unwrap_or_else(|| Path::new("")).to_path_buf()
                }
                Some("self") => {
                    segs.remove(0);
                    dir.to_path_buf()
                }
                // A plain first segment is almost always an external crate.
                _ => return None,
            };
            let mut cands = Vec::new();
            while !segs.is_empty() {
                let joined: std::path::PathBuf = base.join(segs.join("/"));
                cands.push(std::path::PathBuf::from(format!(
                    "{}.rs",
                    joined.to_string_lossy()
                )));
                cands.push(joined.join("mod.rs"));
                segs.pop();
            }
            first(cands)
        }
        _ => None,
    }
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

    fn pb(s: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(s)
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn import_links_ts_forms_and_resolution() {
        let src = "import { a } from './x';\nexport { b } from '../y';\nconst c = require('./w');\nimport('./lazy');\nimport npm from 'react';\n";
        let links = import_links(src, Lang::Ts);
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["./x", "../y", "./w", "./lazy", "react"]);
        // Ranges exclude the quotes.
        for l in &links {
            assert_eq!(&src[l.range.clone()], l.target);
        }
        let files = [pb("app/x.ts"), pb("y/index.tsx"), pb("app/w/index.js")];
        let cur = pb("app/main.ts");
        assert_eq!(
            resolve_import(&links[0], Lang::Ts, &cur, &files),
            Some(pb("app/x.ts"))
        );
        assert_eq!(
            resolve_import(&links[1], Lang::Ts, &cur, &files),
            Some(pb("y/index.tsx")),
            "../y → index file"
        );
        assert_eq!(
            resolve_import(&links[2], Lang::Ts, &cur, &files),
            Some(pb("app/w/index.js"))
        );
        assert_eq!(
            resolve_import(&links[4], Lang::Ts, &cur, &files),
            None,
            "bare specifier = npm package"
        );
    }

    #[cfg(feature = "python")]
    #[test]
    fn import_links_python_forms_and_resolution() {
        let src = "import os\nimport pkg.mod\nfrom .sibling import x\nfrom ..up import y\n";
        let links = import_links(src, Lang::Python);
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["os", "pkg.mod", ".sibling", "..up"]);
        let files = [
            pb("pkg/mod.py"),
            pb("pkg/sub/sibling.py"),
            pb("pkg/up/__init__.py"),
        ];
        let cur = pb("pkg/sub/main.py");
        assert_eq!(resolve_import(&links[0], Lang::Python, &cur, &files), None);
        assert_eq!(
            resolve_import(&links[1], Lang::Python, &cur, &files),
            Some(pb("pkg/mod.py")),
            "absolute module from the project root"
        );
        assert_eq!(
            resolve_import(&links[2], Lang::Python, &cur, &files),
            Some(pb("pkg/sub/sibling.py")),
            ". = the current package"
        );
        assert_eq!(
            resolve_import(&links[3], Lang::Python, &cur, &files),
            Some(pb("pkg/up/__init__.py")),
            ".. walks one package up"
        );
    }

    #[test]
    fn import_links_rust_forms_and_resolution() {
        let src = "mod widgets;\nmod inline { }\nuse crate::views::browse;\nuse super::shared;\nuse std::path::Path;\nuse crate::util::{a, b};\nuse super::*;\n";
        let links = import_links(src, Lang::Rust);
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "widgets",
                "crate::views::browse",
                "super::shared",
                "std::path::Path",
                "crate::util",
                "super"
            ],
            "inline mod (has a body) is not a link; use lists/wildcards link their path prefix"
        );
        let files = [
            pb("src/views/widgets.rs"),
            pb("src/views/browse.rs"),
            pb("src/shared.rs"),
            pb("src/util/mod.rs"),
        ];
        let cur = pb("src/views/mod.rs");
        assert_eq!(
            resolve_import(&links[0], Lang::Rust, &cur, &files),
            Some(pb("src/views/widgets.rs")),
            "mod → sibling file"
        );
        assert_eq!(
            resolve_import(&links[1], Lang::Rust, &cur, &files),
            Some(pb("src/views/browse.rs")),
            "crate:: from src/"
        );
        assert_eq!(
            resolve_import(&links[2], Lang::Rust, &cur, &files),
            Some(pb("src/shared.rs")),
            "super:: from the parent module"
        );
        assert_eq!(
            resolve_import(&links[3], Lang::Rust, &cur, &files),
            None,
            "std/external crates never resolve"
        );
        assert_eq!(
            resolve_import(&links[4], Lang::Rust, &cur, &files),
            Some(pb("src/util/mod.rs")),
            "use list prefix → module dir"
        );
    }

    #[test]
    fn definition_sites_cover_declaration_forms() {
        // Rust: items, let patterns, params.
        let rs = "fn hello(count: i32) {\n    let (a, b) = (1, 2);\n}\nstruct Thing;\nconst MAX: u32 = 9;\n";
        let sites = definition_sites(rs, Lang::Rust);
        let names: Vec<&str> = sites.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["hello", "count", "a", "b", "Thing", "MAX"] {
            assert!(names.contains(&want), "Rust missing {want}: {names:?}");
        }
        // Ranges point at the NAME text.
        for (n, r) in definition_sites(rs, Lang::Rust) {
            assert_eq!(&rs[r], n);
        }
        // Python: def/class/assignment/params/for.
        let py = "class Cat:\n    def meow(self, times):\n        volume = times\n";
        let names: Vec<String> = definition_sites(py, Lang::Python)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for want in ["Cat", "meow", "self", "times", "volume"] {
            assert!(names.iter().any(|n| n == want), "Py missing {want}");
        }
        // Unsupported langs → empty.
        assert!(definition_sites("a = 1", Lang::Yaml).is_empty());
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn definition_sites_ts_declarators_params_methods() {
        let ts = "const { a, b: renamed } = obj;\nfunction go(x: number) {}\nclass K { run() {} }\nconst f = (y) => y;\ntype Alias = string;\n";
        let names: Vec<String> = definition_sites(ts, Lang::Ts)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for want in ["a", "renamed", "go", "x", "K", "run", "f", "y", "Alias"] {
            assert!(
                names.iter().any(|n| n == want),
                "TS missing {want}: {names:?}"
            );
        }
        // Destructuring KEYS are not definitions (only bound values).
        assert!(!names.iter().any(|n| n == "b"), "key `b` must not bind");
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn import_bindings_map_names_to_their_links() {
        let ts = "import Def, { a, b as c } from './x';\nimport * as ns from './y';\n";
        let binds = import_bindings(ts, Lang::Ts);
        let names: Vec<&str> = binds.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["Def", "a", "c", "ns"] {
            assert!(
                names.contains(&want),
                "TS binding missing {want}: {names:?}"
            );
        }
        assert!(!names.contains(&"b"), "`b as c` binds c, not b");
        assert!(binds
            .iter()
            .all(|(n, l)| { (l.target == "./x") == ["Def", "a", "c"].contains(&n.as_str()) }));

        let rs = "use crate::views::browse;\nuse crate::util::{a, b as c};\nmod widgets;\n";
        let binds = import_bindings(rs, Lang::Rust);
        let names: Vec<&str> = binds.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["browse", "a", "c", "widgets"] {
            assert!(
                names.contains(&want),
                "Rust binding missing {want}: {names:?}"
            );
        }

        let py = "import os.path\nfrom pkg.mod import thing as t, other\n";
        let binds = import_bindings(py, Lang::Python);
        let names: Vec<&str> = binds.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["os", "t", "other"] {
            assert!(
                names.contains(&want),
                "Py binding missing {want}: {names:?}"
            );
        }
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `definition_sites` + `import_bindings` recompute per keystroke alongside
    /// `import_links` when a pack's ⌘-click navigation is on.
    #[test]
    fn perf_definition_sites_large_file_stays_fast() {
        let unit =
            "fn f(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\nuse crate::views::browse;\n";
        let src = unit.repeat(1200); // ~6000 lines
        let start = std::time::Instant::now();
        let defs = definition_sites(&src, Lang::Rust);
        let binds = import_bindings(&src, Lang::Rust);
        let elapsed = start.elapsed();
        assert!(!defs.is_empty() && !binds.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "definition_sites+bindings on ~6000 lines took {elapsed:?} (budget 2s)"
        );
    }

    #[test]
    fn import_links_only_for_supported_langs() {
        assert!(import_links("import x\n", Lang::PlainText).is_empty());
        assert!(import_links("@import 'x';\n", Lang::Css).is_empty());
        assert!(import_links("source ./x.sh\n", Lang::Bash).is_empty());
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `import_links` runs on every keystroke (alongside `highlight` +
    /// `fold_regions`) when a language's cmd-click links are on, so it must stay
    /// parse-speed on a large, import-heavy file.
    #[test]
    fn perf_import_links_large_file_stays_fast() {
        let unit = "use crate::views::browse;\nmod widgets;\nfn f(x: i32) -> i32 { x + 1 }\n";
        let src = unit.repeat(1500); // ~4500 lines, 3000 links
        let start = std::time::Instant::now();
        let links = import_links(&src, Lang::Rust);
        let elapsed = start.elapsed();
        assert_eq!(links.len(), 3000);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "import_links on ~4500 lines took {elapsed:?} (budget 2s) — perf regression?"
        );
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
