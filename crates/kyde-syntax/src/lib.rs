//! Tree-sitter syntax highlighting, the way Zed does it: parse with a grammar,
//! run the grammar's HIGHLIGHTS_QUERY, map capture names → theme colors.
//!
//! This is plain Rust (no gpui) and unit-testable. It turns source text into a
//! flat list of colored spans the UI can render.

use kyde_theme as theme;
use std::path::Path;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// The capture names we recognize. Order matters: indices are referenced by the
/// HighlightConfiguration. Anything a grammar emits outside this set falls back
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

fn capture_color(name: &str) -> gpui::Rgba {
    let t = theme::get();
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
    pub start: usize,
    pub end: usize,
    pub color: gpui::Rgba,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lang {
    Tsx,
    Ts,
    Js,
    Rust,
    Json,
    Markdown,
    Bash,
    Css,
    Scss,
    Yaml,
    Toml,
    Python,
    Html,
    Go,
    R,
    Latex,
    Env,
    Gitignore,
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
            Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Lang::Js,
            Some("rs") => Lang::Rust,
            Some("json") => Lang::Json,
            Some("md") | Some("markdown") => Lang::Markdown,
            Some("sh") | Some("bash") | Some("zsh") => Lang::Bash,
            Some("css") => Lang::Css,
            Some("scss") => Lang::Scss,
            Some("yml") | Some("yaml") => Lang::Yaml,
            Some("toml") => Lang::Toml,
            Some("py") | Some("pyi") => Lang::Python,
            Some("html") | Some("htm") => Lang::Html,
            Some("go") => Lang::Go,
            Some("r") | Some("R") => Lang::R,
            Some("tex") | Some("sty") | Some("cls") | Some("latex") => Lang::Latex,
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

    fn config(self) -> Option<HighlightConfiguration> {
        // Each arm is gated to its grammar's Cargo feature. In a feature-trimmed
        // build the absent arms vanish and the lang falls through to the catch-all
        // `_ => return None` (= PlainText), reusing the exact same path as an
        // un-installed pack. The catch-all also covers the builtin-highlighter
        // langs (Env/Gitignore) and PlainText, so the match stays exhaustive
        // regardless of which features are on.
        #[allow(unreachable_patterns)]
        let (lang, highlights, injections, locals) = match self {
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
    /// line-highlighters and PlainText.
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
        .map(|b| matches!(b, b'{' | b'[' | b'(' | b'<'))
        .unwrap_or(false);
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
    let mut spans = Vec::new();
    for (start, line) in lines_with_offsets(source) {
        if line.trim_start().starts_with('#') {
            spans.push(Span {
                start,
                end: start + line.len(),
                color: theme::get().syn_comment,
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
    let events = match hl.highlight(&config, source.as_bytes(), None, |_| None) {
        Ok(e) => e,
        Err(_) => return spans,
    };

    let mut stack: Vec<usize> = Vec::new();
    for ev in events.flatten() {
        match ev {
            HighlightEvent::HighlightStart(h) => stack.push(h.0),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let color = match stack.last() {
                    Some(&idx) => capture_color(CAPTURES[idx]),
                    None => theme::get().text,
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
