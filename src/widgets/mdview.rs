//! Selectable rendered-markdown view for the split preview's right pane.
//!
//! A custom element shapes each parsed block with its own font size (so headings stay large),
//! and owns the wrapped layouts so a click-drag maps to a global byte selection *across* blocks.
//! The selection is painted by setting `background_color` on the selected text runs — gpui then
//! draws the highlight across wrapped rows for free, so there's no manual selection-rect math.
//! ⌘C copies the selected text.
//!
//! Modeled on `editor.rs` (entity + custom `Element`, mouse handlers on the wrapping div, layout
//! cached back onto the entity for hit-testing).

use crate::markdown::{self, Block, Span};
use crate::theme;
use gpui::prelude::*;
use gpui::{
    actions, div, img, point, px, App, Bounds, ClipboardItem, Context, Element, ElementId, Entity,
    FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, SharedString, Size, Style, TextRun, Window, WrappedLine,
};
use std::ops::Range;

const CONTEXT: &str = "MarkdownView";
actions!(mdview, [Copy, SelectAll]);

/// Bind ⌘C / ⌘A for the markdown preview. Called once at startup.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT)),
    ]);
}

/// One renderable block (heading / paragraph / list item / quote / code / rule).
struct MdBlock {
    text: SharedString,
    /// Base text runs (no selection background) — cloned + split per frame to apply selection.
    runs: Vec<TextRun>,
    size: Pixels,
    top_pad: f32,
    indent: f32,
    /// Filled background (code block) painted behind the text.
    bg: Option<Hsla>,
    /// Left accent border (block quote).
    border_left: Option<Hsla>,
    /// Byte offset of this block's text within `doc`.
    byte_start: usize,
}

/// A laid-out block, cached on the entity each paint for mouse hit-testing.
#[derive(Clone)]
struct LaidBlock {
    line: WrappedLine,
    origin: Point<Pixels>,
    line_height: Pixels,
    byte_start: usize,
    text_len: usize,
}

pub struct MarkdownView {
    /// Raw markdown source last parsed — re-parse only when it changes, so selection survives
    /// re-renders that don't edit the text.
    src: String,
    blocks: Vec<MdBlock>,
    /// Flattened plain text (block texts joined by `\n`); selection offsets index into this.
    doc: String,
    /// Selection as a byte range in `doc` (normalized so `start <= end`).
    sel: Range<usize>,
    /// Drag anchor (byte offset) while selecting.
    anchor: usize,
    selecting: bool,
    /// Per-block layout from the last paint, for hit-testing in the mouse handlers. Now
    /// accumulated across every text segment each frame (cleared in `render`), so a doc
    /// with images — split into multiple text elements — still hit-tests as one document.
    laid: Vec<LaidBlock>,
    /// Document-ordered layout: runs of text blocks interleaved with images, so images
    /// render in place. Text runs are index ranges into `blocks`.
    layout: Vec<MdItem>,
    /// Directory of the open markdown file, for resolving relative image `src` paths.
    base_dir: Option<std::path::PathBuf>,
    focus_handle: FocusHandle,
}

/// One entry in the document-ordered render list.
enum MdItem {
    /// A maximal run of consecutive text blocks (indices into `blocks`).
    Text(Range<usize>),
    /// An image, rendered as its own `img()` element in place.
    Image { src: String, alt: String },
}

impl MarkdownView {
    pub fn new(text: &str, base_dir: Option<std::path::PathBuf>, cx: &mut Context<Self>) -> Self {
        let (blocks, doc, layout) = build(text);
        Self {
            src: text.to_string(),
            blocks,
            doc,
            sel: 0..0,
            anchor: 0,
            selecting: false,
            laid: Vec::new(),
            layout,
            base_dir,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Re-parse only if the source actually changed (preserves selection otherwise). The
    /// base dir can change without the text changing (switching between two files of equal
    /// content is unlikely, but keep it in sync regardless).
    pub fn set_text(
        &mut self,
        text: &str,
        base_dir: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.base_dir = base_dir;
        if self.src == text {
            return;
        }
        let (blocks, doc, layout) = build(text);
        self.src = text.to_string();
        self.blocks = blocks;
        self.doc = doc;
        self.layout = layout;
        self.sel = 0..0;
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.sel.start != self.sel.end {
            if let Some(s) = self.doc.get(self.sel.clone()) {
                cx.write_to_clipboard(ClipboardItem::new_string(s.to_string()));
            }
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = 0..self.doc.len();
        cx.notify();
    }

    /// Byte offset in `doc` nearest to a window-coord position (clamped to the document).
    /// `laid` origins are absolute window coords, so the mouse position is compared directly.
    fn offset_for_position(&self, pos: Point<Pixels>) -> usize {
        let local = pos;
        let mut best = 0usize;
        for b in &self.laid {
            // Above this block → clamp to its start; inside → exact; else keep scanning.
            let top = b.origin.y;
            let bottom = b.origin.y + b.line.size(b.line_height).height;
            if local.y < top {
                return b.byte_start;
            }
            if local.y <= bottom {
                let within = point(local.x - b.origin.x, local.y - b.origin.y);
                let idx = b
                    .line
                    .closest_index_for_position(within, b.line_height)
                    .unwrap_or_else(|e| e);
                return b.byte_start + idx.min(b.text_len);
            }
            best = b.byte_start + b.text_len;
        }
        best
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let off = self.offset_for_position(ev.position);
        self.anchor = off;
        self.sel = off..off;
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && ev.dragging() {
            let off = self.offset_for_position(ev.position);
            self.sel = self.anchor.min(off)..self.anchor.max(off);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
}

impl Focusable for MarkdownView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(gpui::CursorStyle::IBeam)
            // `w_full` (not `size_full`): take the full width but grow to the rendered
            // content's height, so the enclosing scroll container actually overflows and
            // the shared `with_scrollbars` thumb appears. `size_full` pinned it to the
            // viewport height → no overflow → no scrollbar (and no scrolling).
            .w_full()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .flex()
            .flex_col()
            // Hit-test cache is rebuilt every frame: each text segment appends its laid blocks
            // during paint, so reset here before they do.
            .child({
                self.laid.clear();
                div().w_full().flex().flex_col().children(
                    self.layout
                        .iter()
                        .map(|item| self.render_item(item, cx))
                        .collect::<Vec<_>>(),
                )
            })
    }
}

impl MarkdownView {
    /// Render one document item: a run of text blocks (custom element) or an image.
    fn render_item(&self, item: &MdItem, cx: &mut Context<Self>) -> gpui::AnyElement {
        match item {
            MdItem::Text(range) => MarkdownElement {
                view: cx.entity(),
                range: range.clone(),
            }
            .into_any_element(),
            MdItem::Image { src, alt } => self.render_image(src, alt),
        }
    }

    /// Resolve an image `src` (relative to the markdown file, or an http URL) and render it.
    /// Local files render directly; remote URLs render only when built with `remote-images`,
    /// else fall back to the alt text so the layout still reads sensibly.
    fn render_image(&self, src: &str, alt: &str) -> gpui::AnyElement {
        let remote = src.starts_with("http://") || src.starts_with("https://");
        let wrap = || div().w_full().flex().justify_center().py_2();
        if remote {
            if cfg!(feature = "remote-images") {
                return wrap()
                    .child(img(SharedString::from(src.to_string())).max_w_full())
                    .into_any_element();
            }
        } else if let Some(dir) = &self.base_dir {
            let path = dir.join(src);
            if path.exists() {
                return wrap().child(img(path).max_w_full()).into_any_element();
            }
        }
        // Couldn't load — show the alt text (or the src) as a dim placeholder.
        let label = if alt.is_empty() { src } else { alt };
        wrap()
            .child(
                div()
                    .text_color(theme::get().line_number)
                    .child(SharedString::from(format!("🖼 {label}"))),
            )
            .into_any_element()
    }
}

// ── parsing → renderable blocks ──────────────────────────────────────────────

fn mk_font(bold: bool, italic: bool, code: bool) -> Font {
    Font {
        family: if code {
            theme::font::FAMILY
        } else {
            theme::font::UI_FAMILY
        }
        .into(),
        features: Default::default(),
        fallbacks: None,
        weight: if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
    }
}

/// Build runs + concatenated text for a block's inline spans.
fn spans_to_runs(spans: &[Span], color: Hsla, bold_all: bool) -> (String, Vec<TextRun>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    for sp in spans {
        let c: Hsla = if sp.code {
            gpui::rgb(0xC9CDD6).into()
        } else {
            color
        };
        runs.push(TextRun {
            len: sp.text.len(),
            font: mk_font(sp.bold || bold_all, sp.italic, sp.code),
            color: c,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        text.push_str(&sp.text);
    }
    (text, runs)
}

fn build(src: &str) -> (Vec<MdBlock>, String, Vec<MdItem>) {
    let t = theme::get();
    let mut blocks = Vec::new();
    let mut doc = String::new();
    // Image split points: `(block index at which the image appears, src, alt)`.
    let mut images: Vec<(usize, String, String)> = Vec::new();
    let push = |blocks: &mut Vec<MdBlock>,
                doc: &mut String,
                text: String,
                runs: Vec<TextRun>,
                size: f32,
                top_pad: f32,
                indent: f32,
                bg: Option<Hsla>,
                border_left: Option<Hsla>| {
        let byte_start = doc.len();
        doc.push_str(&text);
        doc.push('\n');
        blocks.push(MdBlock {
            text: SharedString::from(text),
            runs,
            size: px(size),
            top_pad,
            indent,
            bg,
            border_left,
            byte_start,
        });
    };
    for b in markdown::parse(src) {
        match b {
            Block::Heading(level, spans) => {
                let size = match level {
                    1 => 26.0,
                    2 => 21.0,
                    3 => 18.0,
                    _ => 15.0,
                };
                let (text, runs) = spans_to_runs(&spans, t.text.into(), true);
                push(
                    &mut blocks,
                    &mut doc,
                    text,
                    runs,
                    size,
                    10.0,
                    0.0,
                    None,
                    None,
                );
            }
            Block::Paragraph(spans) => {
                let (text, runs) = spans_to_runs(&spans, t.text.into(), false);
                push(
                    &mut blocks,
                    &mut doc,
                    text,
                    runs,
                    14.0,
                    4.0,
                    0.0,
                    None,
                    None,
                );
            }
            Block::Code(code) => {
                let runs = vec![TextRun {
                    len: code.len(),
                    font: mk_font(false, false, true),
                    color: t.text.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                push(
                    &mut blocks,
                    &mut doc,
                    code,
                    runs,
                    13.0,
                    4.0,
                    0.0,
                    Some(t.bg_mid.into()),
                    None,
                );
            }
            Block::ListItem(depth, spans) => {
                let (mut text, mut runs) = spans_to_runs(&spans, t.text.into(), false);
                // Prepend a bullet as its own (line-number-colored) run.
                let bullet = "• ";
                runs.insert(
                    0,
                    TextRun {
                        len: bullet.len(),
                        font: mk_font(false, false, false),
                        color: t.line_number.into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    },
                );
                text.insert_str(0, bullet);
                push(
                    &mut blocks,
                    &mut doc,
                    text,
                    runs,
                    14.0,
                    2.0,
                    depth as f32 * 14.0,
                    None,
                    None,
                );
            }
            Block::Quote(spans) => {
                let (text, runs) = spans_to_runs(&spans, t.secondary_text.into(), false);
                push(
                    &mut blocks,
                    &mut doc,
                    text,
                    runs,
                    14.0,
                    4.0,
                    12.0,
                    None,
                    Some(t.divider.into()),
                );
            }
            Block::Rule => {
                push(
                    &mut blocks,
                    &mut doc,
                    String::new(),
                    Vec::new(),
                    8.0,
                    8.0,
                    0.0,
                    None,
                    None,
                );
            }
            // Images render in place as their own elements (see `render`); record the split
            // point (how many text blocks precede this image) so the layout can interleave.
            Block::Image { src, alt } => {
                images.push((blocks.len(), src, alt));
            }
        }
    }
    // Build the document-ordered layout: text runs split at each image.
    let mut layout = Vec::new();
    let mut cursor = 0usize;
    for (at, src, alt) in images {
        if at > cursor {
            layout.push(MdItem::Text(cursor..at));
        }
        layout.push(MdItem::Image { src, alt });
        cursor = at;
    }
    if cursor < blocks.len() {
        layout.push(MdItem::Text(cursor..blocks.len()));
    }
    (blocks, doc, layout)
}

/// Clone a block's runs, applying the selection background to the sub-range `[a, b)` (block-local
/// byte offsets), splitting runs at the boundaries so the highlight aligns to characters.
fn runs_with_selection(base: &[TextRun], a: usize, b: usize, sel_bg: Hsla) -> Vec<TextRun> {
    if a >= b {
        return base.to_vec();
    }
    let mut out = Vec::with_capacity(base.len() + 2);
    let mut pos = 0usize;
    for run in base {
        let start = pos;
        let end = pos + run.len;
        pos = end;
        // Split this run into [start,a) [a,b) [b,end) intersected with [start,end).
        let segs = [
            (start, a.clamp(start, end), false),
            (a.clamp(start, end), b.clamp(start, end), true),
            (b.clamp(start, end), end, false),
        ];
        for (s, e, hot) in segs {
            if e > s {
                let mut r = run.clone();
                r.len = e - s;
                if hot {
                    r.background_color = Some(sel_bg);
                }
                out.push(r);
            }
        }
    }
    out
}

// ── the painting element ─────────────────────────────────────────────────────

struct MarkdownElement {
    view: Entity<MarkdownView>,
    /// Which run of text blocks (indices into `view.blocks`) this element renders.
    range: Range<usize>,
}

impl IntoElement for MarkdownElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

struct MdPrepaint {
    laid: Vec<LaidBlock>,
    /// `(bounds, color)` background fills (code blocks) and `(bounds, color)` left borders.
    fills: Vec<(Bounds<Pixels>, Hsla)>,
}

impl Element for MarkdownElement {
    type RequestLayoutState = ();
    type PrepaintState = MdPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        // Height depends on wrap width (only known at layout), so measure: shape every block at
        // the available width and sum their heights + paddings.
        let view = self.view.clone();
        let self_range = self.range.clone();
        let mut style = Style::default();
        style.size.width = gpui::relative(1.0).into();
        let layout_id =
            window.request_measured_layout(style, move |_known, available, window, cx| {
                let avail_w = match available.width {
                    gpui::AvailableSpace::Definite(w) => w,
                    _ => px(400.0),
                };
                let v = view.read(cx);
                let mut total = 0.0f32;
                let range = self_range.clone();
                for b in &v.blocks[range] {
                    let lh = b.size * 1.45;
                    let wrap = (f32::from(avail_w) - b.indent).max(40.0);
                    let lines = window
                        .text_system()
                        .shape_text(b.text.clone(), b.size, &b.runs, Some(px(wrap)), None)
                        .unwrap_or_default();
                    let h: f32 = lines
                        .iter()
                        .map(|l| f32::from(l.size(lh).height))
                        .sum::<f32>()
                        .max(f32::from(lh));
                    total += b.top_pad + h;
                }
                Size {
                    width: avail_w,
                    height: px(total + 16.0),
                }
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> MdPrepaint {
        let v = self.view.read(cx);
        let sel = v.sel.clone();
        let sel_bg: Hsla = theme::get().selected_bg.into();
        let pad_x = px(20.0);
        let mut y = bounds.top() + px(8.0);
        let range = self.range.clone();
        let mut laid = Vec::with_capacity(range.len());
        let mut fills = Vec::new();
        for b in &v.blocks[range] {
            y += px(b.top_pad);
            let lh = b.size * 1.45;
            let indent = pad_x + px(b.indent);
            let wrap =
                (f32::from(bounds.size.width) - f32::from(indent) - f32::from(pad_x)).max(40.0);
            // Selection sub-range within this block (block-local bytes).
            let bs = b.byte_start;
            let be = bs + b.text.len();
            let (la, lb) = (sel.start.clamp(bs, be) - bs, sel.end.clamp(bs, be) - bs);
            let runs = runs_with_selection(&b.runs, la, lb, sel_bg);
            let lines = window
                .text_system()
                .shape_text(b.text.clone(), b.size, &runs, Some(px(wrap)), None)
                .unwrap_or_default();
            let line = lines.into_iter().next().unwrap_or_default();
            let h = f32::from(line.size(lh).height).max(f32::from(lh));
            let origin = point(bounds.left() + indent, y);
            if let Some(c) = b.bg {
                fills.push((
                    Bounds::new(
                        point(bounds.left() + pad_x, y - px(4.0)),
                        gpui::size(bounds.size.width - pad_x * 2.0, px(h + 8.0)),
                    ),
                    c,
                ));
            }
            if let Some(c) = b.border_left {
                fills.push((
                    Bounds::new(point(bounds.left() + pad_x, y), gpui::size(px(2.0), px(h))),
                    c,
                ));
            }
            laid.push(LaidBlock {
                line,
                origin,
                line_height: lh,
                byte_start: b.byte_start,
                text_len: b.text.len(),
            });
            y += px(h);
        }
        MdPrepaint { laid, fills }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut MdPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (b, c) in &pre.fills {
            window.paint_quad(gpui::fill(*b, *c));
        }
        for b in &pre.laid {
            // `paint` only draws glyphs; the selection highlight lives in the runs'
            // `background_color`, which `paint_background` draws (behind the text).
            let _ = b.line.paint_background(
                b.origin,
                b.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
            let _ = b.line.paint(
                b.origin,
                b.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        // Append this segment's layout to the entity's hit-test cache (cleared each frame in
        // `render`), so every text segment together forms one hit-testable document.
        let laid = pre.laid.clone();
        self.view.update(cx, |v, _| v.laid.extend(laid));
    }
}
