//! The Layers panel: a row for every layer, sublayer, group and object, nested and indented, with
//! the eye (visibility) and lock columns, the layer colour bar, a disclosure triangle, a thumbnail
//! and the name, then the target circle and the selection column; a bottom bar with the panel's
//! buttons and the ≡ menu.
//!
//! - Clicking a row highlights it and makes its layer current (`layer.setCurrent`); Shift-click
//!   highlights the range of rows down to it, Ctrl/Cmd-click toggles one (`layer.highlight`). Panel
//!   commands (Duplicate, Delete, Merge, Options…) act on the highlighted rows.
//! - The selection column selects a row's art (`layer.selectAll`, Shift adds or removes); a row
//!   whose art is selected shows a square in its layer's colour, and dragging that square onto
//!   another row moves the selected art there (Alt copies it).
//! - The eye and lock columns toggle a row (`layer.setProps`); dragging down a column sets every
//!   row it passes to the same state, in one undo step. Ctrl/Cmd-click an eye switches the layer
//!   between Preview and Outline. Alt-click an eye hides the other layers (`layer.hideOthers`), or
//!   shows every layer when they are hidden (`layer.showAll`); Alt-click a lock does the same with
//!   `layer.lockOthers` and `layer.unlockAll`. Ctrl/Cmd+Alt-click an eye switches the other layers
//!   to Outline and the row's layer to Preview (`layer.outlineOthers`), or switches every layer to
//!   Preview when the others are in Outline (`layer.previewAll`).
//! - Dragging rows moves them above, below or into a layer or group (`layer.move`, a drop line or
//!   a box shows where); Alt copies them. A dimmed copy of the row follows the pointer. Layers
//!   dropped in the empty space under the last row go below the bottom layer. Dropped on the trash
//!   they are deleted, on New Layer duplicated.
//! - The triangle opens or closes a row; Alt-click does the same to everything inside it.
//! - Double-clicking a name renames it; double-clicking elsewhere on a row opens its options.
//! - A target circle targets its layer, group or object (`layer.target`); dragging it onto another
//!   row's circle moves the appearance there (Alt copies it, `appearance.transfer`) and dropping it
//!   on the trash clears it.
//! - Search All and the Filter button at its right (`ui.layersFilter`, lit while a filter is on)
//!   narrow the rows, and every row that holds others shows open while they do. A row stays when
//!   its name holds the text typed, it is one of the object kinds ticked under By Object and it is
//!   in one of the states ticked under By Layer (a section with nothing ticked takes every row), or
//!   when a row inside it stays. Neither changes the document, its open rows or the selection.
//!
//! While an opacity mask is edited the panel lists only its art, under an `<Opacity Mask>` entry.
//!
//! The rows open are the document's ([`OpenRows`], saved in native files): a document opens with
//! only its top-level layers open. The rows shown are listed once per document change, and only
//! those scrolled into view are laid out, so a document of many thousand objects costs no more
//! per frame than a small one.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use egui::{Color32, Sense, Stroke, StrokeKind, Ui, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vectorcraft_doc::{Document, LiveShape, Node, NodeId, NodeKind};
use vectorcraft_engine::OpenRows;

use crate::theme::Tokens;
use crate::thumb_worker::{Settle, ThumbWorker};
use crate::widgets::{Live, PanelDrag};
use crate::{VectorcraftApp, icons, widgets};

/// Width of the eye and the lock columns.
const COLUMN: f32 = 25.0;
/// Indent per nesting level.
const INDENT: f32 = 14.0;
/// Right edge offsets of the target circle and the selection square.
const TARGET_X: f32 = 28.0;
const SQUARE_X: f32 = 11.0;

/// Layers panel options (≡ › Panel Options…), kept with the UI state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PanelOptions {
    /// Show Layers Only: no rows for groups and objects.
    pub layers_only: bool,
    /// Locate Selected Objects (off by default): when the last selected object changes outside the
    /// panel, on the canvas or by a command, the panel opens the rows around it and scrolls to its
    /// row, as after Locate Object, without highlighting the row. When Show Layers Only, a search or
    /// the filter leaves the object without a row, the panel opens no rows for it.
    pub locate_selected: bool,
    /// Row height in points: [`ROW_SMALL`], [`ROW_MEDIUM`], [`ROW_LARGE`] or another (12–100).
    pub row_size: f32,
    /// Thumbnails on layer, group and object rows.
    pub thumb_layers: bool,
    pub thumb_groups: bool,
    pub thumb_objects: bool,
}

pub const ROW_SMALL: f32 = 20.0;
pub const ROW_MEDIUM: f32 = 26.0;
pub const ROW_LARGE: f32 = 40.0;

impl Default for PanelOptions {
    fn default() -> Self {
        Self { layers_only: false, locate_selected: false, row_size: ROW_MEDIUM, thumb_layers: true, thumb_groups: true, thumb_objects: true }
    }
}

impl PanelOptions {
    /// The row height, kept to 12–100 points.
    pub fn row(&self) -> f32 {
        if self.row_size.is_finite() { self.row_size.clamp(12.0, 100.0) } else { ROW_MEDIUM }
    }
    /// Whether `n`'s row shows a thumbnail.
    fn thumb(&self, n: &Node) -> bool {
        self.row() >= 18.0
            && match n.kind {
                NodeKind::Layer { .. } => self.thumb_layers,
                NodeKind::Group { .. } => self.thumb_groups,
                _ => self.thumb_objects,
            }
    }
}

/// What a drag in the panel carries.
#[derive(Clone, Debug)]
enum LayersDrag {
    /// Rows dragged to move (or, with Alt, copy) them.
    Rows(Vec<NodeId>),
    /// The selected-art square: the selected objects go to the row it is dropped on.
    Art,
    /// Ruler guide rows (indexes into the document's guides): they go to the layer of the row
    /// they are dropped on.
    Guides(Vec<usize>),
}

/// A drag down the eye or lock column: every row it passes takes `value`.
#[derive(Clone, Debug, Default)]
struct ColumnDrag {
    /// The eye column (visibility), else the lock column.
    eye: bool,
    value: bool,
    ids: Vec<u64>,
}

/// Stable id for the layers panel's per-name UI state.
fn key(name: &str) -> egui::Id {
    egui::Id::unique(("layers-panel", name))
}

/// Temp-memory key for the layer rename editor.
fn rename_id() -> egui::Id {
    egui::Id::unique("layers-rename")
}

/// Ghost rows are drawn at this opacity.
const GHOST_OPACITY: f32 = 0.5;

/// The colour of the layer holding `id`.
fn layer_colour(doc: &Document, id: NodeId) -> Color32 {
    let [r, g, b] = doc.layer_color(id);
    Color32::from_rgb(r, g, b)
}

/// The layer colour bar of row `n` (rect `r`) at `x`: wider on layers.
fn colour_bar(ui: &Ui, n: &Node, r: egui::Rect, x: f32, colour: Color32) {
    let w = if n.is_layer() { 4.0 } else { 2.0 };
    ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(x - 1.0, r.top()), vec2(w, r.height())), 0.0, colour);
}

/// The thumbnail of row `n` (rect `r`) at `x`; returns where it went.
fn row_thumb(ui: &Ui, view: &View, n: &Arc<Node>, r: egui::Rect, x: f32) -> egui::Rect {
    let s = (view.h - 4.0).min(40.0);
    let th = egui::Rect::from_min_size(egui::pos2(x, r.center().y - s / 2.0), vec2(s, s));
    ui.painter().rect_filled(th, 0.0, Color32::WHITE);
    ui.painter().rect_stroke(th, 0.0, Stroke::new(1.0, view.t.border), StrokeKind::Outside);
    if !real_thumb(ui, &view.thumbs, n, th) {
        thumb(ui, n, th);
    }
    th
}

/// The dragged row `src`, dimmed, following the pointer at `pos` above everything else: its
/// colour bar, thumbnail and name.
fn drag_ghost(ui: &Ui, view: &View, src: NodeId, pos: egui::Pos2) {
    let (doc, t, h) = (view.doc, &view.t, view.h);
    let Some(n) = node_arc(doc, src) else { return };
    let n = &n;
    let (grab, width, depth) = ui.data(|d| d.get_temp::<(egui::Vec2, f32, usize)>(key("grab"))).unwrap_or((vec2(h, h / 2.0), 240.0, 0));
    // A plain Ui on the tooltip layer, not an Area: a new Area is invisible for a frame and fades in.
    let r = egui::Rect::from_min_size(pos - grab, vec2(width, h));
    let id = key("ghost");
    let mut ui = Ui::new(ui.ctx().clone(), id, egui::UiBuilder::new().layer_id(egui::LayerId::new(egui::Order::Tooltip, id)).max_rect(r));
    ui.set_opacity(GHOST_OPACITY);
    let p = ui.painter();
    p.rect_filled(r, 0.0, t.row_selected);
    p.rect_stroke(r, 0.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
    let mut x = r.left() + 2.0 * COLUMN + 2.0;
    colour_bar(&ui, n, r, x, layer_colour(doc, src));
    x += 22.0 + depth as f32 * INDENT;
    if view.opts.thumb(n) {
        x = row_thumb(&ui, view, n, r, x).right() + 4.0;
    }
    let label = n.identify_label(view.identify_xml_id);
    let name = if view.identify_xml_id { label } else { painted_name(n, &label, crate::i18n::current()) };
    ui.painter().text(egui::pos2(x, r.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), t.text);
}

/// Where a dragged row goes relative to the row it is dropped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Above,
    Below,
    Inside,
}

impl Place {
    fn id(self) -> &'static str {
        match self {
            Place::Above => "above",
            Place::Below => "below",
            Place::Inside => "inside",
        }
    }
}

/// The name painted for a row in `lang`: a generated `<Kind>` name is translated, anything else is
/// user data (an unnamed text object shows its text, which can look like `<Path>`; only an empty
/// one is called `<Text>`).
fn painted_name(n: &Node, name: &str, lang: crate::i18n::Lang) -> String {
    let generated = n.name.is_none() && !matches!(&n.kind, NodeKind::Text(t) if t.runs.iter().any(|r| !r.text.is_empty()));
    if generated && let Some(inner) = name.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        return format!("<{}>", crate::i18n::tr(lang, inner));
    }
    name.to_string()
}

/// The name the panel shows for `n` in `lang`: its XML ID while Identify Objects By is XML ID,
/// otherwise [`painted_name`].
pub(crate) fn shown_name(app: &VectorcraftApp, n: &Node, lang: crate::i18n::Lang) -> String {
    let xml_id = app.session.prefs.identify_objects_by == "xmlId";
    let label = n.identify_label(xml_id);
    if xml_id { label } else { painted_name(n, &label, lang) }
}

/// Whether `n` gets rows for what it holds (a compound path's paths are one object).
fn opens(n: &Node, opts: &PanelOptions) -> bool {
    match n.kind {
        NodeKind::Compound { .. } => false,
        _ => n.children().is_some_and(|c| c.iter().any(|c| !opts.layers_only || c.is_layer())),
    }
}

/// One row of the panel.
#[derive(Clone)]
struct Row {
    node: Arc<Node>,
    /// Nesting level (0: a top-level layer).
    depth: usize,
    /// A clipping path: its group or layer clips.
    clip_path: bool,
    /// It holds rows ([`opens`]), and shows them.
    opens: bool,
    open: bool,
    /// The colour of its layer (its own, for a layer).
    colour: Color32,
    /// Drawn dimmed: hidden itself or in a hidden layer or group, or a template layer.
    dim: bool,
    /// A ruler guide on the layer `node` (its index in the document's guides): the guide's row.
    guide: Option<usize>,
}

/// An object kind under the Filter's By Object (`ui.layersFilter`'s `objects`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Paths,
    Shapes,
    Text,
    Images,
    Effects,
    Groups,
    LinkedFiles,
    Guides,
}

impl ObjectKind {
    /// Every kind with its parameter name, in the popup's order.
    pub const ALL: [(Self, &'static str); 8] = [
        (Self::Paths, "paths"),
        (Self::Shapes, "shapes"),
        (Self::Text, "text"),
        (Self::Images, "images"),
        (Self::Effects, "effects"),
        (Self::Groups, "groups"),
        (Self::LinkedFiles, "linkedFiles"),
        (Self::Guides, "guides"),
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Paths => tl!("Paths"),
            Self::Shapes => tl!("Shapes"),
            Self::Text => tl!("Text"),
            Self::Images => tl!("Images"),
            Self::Effects => tl!("Effects"),
            Self::Groups => tl!("Groups"),
            Self::LinkedFiles => tl!("Linked Files"),
            Self::Guides => tl!("Guides"),
        }
    }

    /// Whether `n`'s row is of this kind. Paths take plain paths, the paths whose corners Live
    /// Corners cut and compound paths; Shapes take live rectangles, ellipses, polygons and lines;
    /// Paths and Shapes leave out paths made into guides. Effects take a row with an effect, shown
    /// or hidden, on itself or on one of its fills or strokes. Linked Files take linked images and
    /// placed documents.
    fn takes(self, n: &Node) -> bool {
        match (self, &n.kind) {
            (Self::Guides, NodeKind::Path { guide, .. }) => *guide,
            (Self::Paths | Self::Shapes, NodeKind::Path { guide: true, .. }) => false,
            (Self::Paths, NodeKind::Path { live, .. }) => live.as_deref().is_none_or(|l| matches!(l, LiveShape::Path { .. })),
            (Self::Shapes, NodeKind::Path { live, .. }) => live.as_deref().is_some_and(|l| !matches!(l, LiveShape::Path { .. })),
            (Self::Paths, NodeKind::Compound { .. })
            | (Self::Text, NodeKind::Text(_))
            | (Self::Images, NodeKind::Image(_))
            | (Self::Groups, NodeKind::Group { .. })
            | (Self::LinkedFiles, NodeKind::PlacedDocument(_)) => true,
            (Self::LinkedFiles, NodeKind::Image(im)) => im.link.is_some(),
            (Self::Effects, _) => !n.appearance.effects.is_empty() || n.appearance.items.iter().any(|i| !i.effects().is_empty()),
            _ => false,
        }
    }
}

/// A row state under the Filter's By Layer (`ui.layersFilter`'s `layers`): the row's own lock or
/// eye, as its lock and eye columns show them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Locked,
    Hidden,
}

impl RowState {
    /// Every state with its parameter name, in the popup's order.
    pub const ALL: [(Self, &'static str); 2] = [(Self::Locked, "locked"), (Self::Hidden, "hidden")];

    fn label(self) -> &'static str {
        match self {
            Self::Locked => tl!("Locked"),
            Self::Hidden => tl!("Hidden"),
        }
    }

    fn takes(self, n: &Node) -> bool {
        match self {
            Self::Locked => n.locked,
            Self::Hidden => !n.visible,
        }
    }
}

/// The Layers panel's Filter (`ui.layersFilter`): the object kinds ticked under By Object and the
/// row states ticked under By Layer, each in the popup's order. A section with nothing ticked takes
/// every row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub objects: Vec<ObjectKind>,
    pub layers: Vec<RowState>,
}

impl Filter {
    /// Whether anything is ticked.
    pub fn is_on(&self) -> bool {
        !self.objects.is_empty() || !self.layers.is_empty()
    }

    /// Whether `n`'s row is of a ticked kind and in a ticked state.
    fn keeps(&self, n: &Node) -> bool {
        (self.objects.is_empty() || self.objects.iter().any(|k| k.takes(n))) && (self.layers.is_empty() || self.layers.iter().any(|s| s.takes(n)))
    }

    /// Whether the ruler guide rows stay: with no filter on, or with Guides ticked and no row state
    /// ticked.
    fn keeps_guides(&self) -> bool {
        !self.is_on() || (self.objects.contains(&ObjectKind::Guides) && self.layers.is_empty())
    }
}

/// The parameter names of the ones of `all` in `on`, in `all`'s order.
fn names<T: Copy + PartialEq>(all: &[(T, &'static str)], on: &[T]) -> Vec<&'static str> {
    all.iter().filter(|(t, _)| on.contains(t)).map(|(_, name)| *name).collect()
}

/// The parameter names of `on` with `t` added, or taken out when it is in `on`.
fn toggled<T: Copy + PartialEq>(all: &[(T, &'static str)], on: &[T], t: T) -> Vec<&'static str> {
    all.iter().filter(|(x, _)| if *x == t { !on.contains(x) } else { on.contains(x) }).map(|(_, name)| *name).collect()
}

/// The list `key` of `p` read by the parameter names of `all`, in `all`'s order: `None` when `p`
/// leaves it out or sets it to null, and an error when it is anything other than an array of
/// those names.
fn parse_names<T: Copy + PartialEq>(p: &Value, key: &str, all: &[(T, &'static str)]) -> Result<Option<Vec<T>>, String> {
    let given = match p.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(|name| all.iter().find(|(_, n)| Some(*n) == name.as_str()).map(|(t, _)| *t)).collect::<Option<Vec<T>>>()),
    };
    let Some(given) = given else {
        let names: Vec<&str> = all.iter().map(|(_, n)| *n).collect();
        return Err(format!("`{key}` must be an array of these names: {}", names.join(", ")));
    };
    Ok(Some(all.iter().map(|(t, _)| *t).filter(|t| given.contains(t)).collect()))
}

/// What the rows were listed from: listed again when any of it changes.
#[derive(Clone, PartialEq)]
struct RowsKey {
    uid: u64,
    revision: u64,
    open: u64,
    layers_only: bool,
    query: String,
    filter: Filter,
}

/// The rows the panel shows, top down: the layers (only the opacity mask's while one is edited)
/// with the rows of the open ones inside them. A search or a filter shows every row whose name
/// contains `query` (lowercase) and that `filter` keeps, and every row holding one.
fn list_rows(doc: &Document, open: &OpenRows, opts: &PanelOptions, query: &str, filter: &Filter) -> Vec<Row> {
    let mask_layer = doc.mask_edit.map(|m| m.layer);
    let mut out = vec![];
    for l in doc.layers.iter().rev().filter(|l| mask_layer.is_none_or(|m| m == l.id)) {
        push_rows(l, 0, false, (Color32::PLACEHOLDER, true), (open, opts, query, filter, doc), &mut out);
    }
    out
}

/// Add the rows of `n` and those open inside it to `out`; `(colour, shown)` are those of what holds
/// it (the layer's colour, and whether it is visible).
fn push_rows(
    n: &Arc<Node>,
    depth: usize,
    clip_path: bool,
    (colour, shown): (Color32, bool),
    cx: (&OpenRows, &PanelOptions, &str, &Filter, &Document),
    out: &mut Vec<Row>,
) {
    let (open_rows, opts, query, filter, doc) = cx;
    if opts.layers_only && !n.is_layer() {
        return;
    }
    let colour = match &n.kind {
        NodeKind::Layer { color, .. } => {
            let [r, g, b] = color.rgb();
            Color32::from_rgb(r, g, b)
        }
        _ => colour,
    };
    let shown = shown && n.visible;
    let searching = !query.is_empty();
    let narrowing = searching || filter.is_on();
    // A layer's ruler guides are rows in it, above its art (not while searching, nor while the
    // filter leaves them out).
    let guides: Vec<usize> =
        if n.is_layer() && !opts.layers_only && !searching && filter.keeps_guides() { doc.guides_on(n.id).map(|(i, _)| i).collect() } else { vec![] };
    let opens = opens(n, opts) || !guides.is_empty();
    let open = opens && (narrowing || open_rows.contains(n.id));
    let at = out.len();
    out.push(Row { node: n.clone(), depth, clip_path, opens, open, colour, dim: !shown || n.is_template(), guide: None });
    if open {
        for i in guides.into_iter().rev() {
            out.push(Row { node: n.clone(), depth: depth + 1, clip_path: false, opens: false, open: false, colour, dim: !shown, guide: Some(i) });
        }
    }
    if open && let Some(children) = n.children() {
        for (i, c) in children.iter().enumerate().rev() {
            push_rows(c, depth + 1, i == 0 && n.clips(), (colour, shown), cx, out);
        }
    }
    // Searching or filtering, a row stays for a match of its own (its name holds the search text
    // and the filter keeps it) or for a row it keeps inside it.
    if narrowing && out.len() == at + 1 && !(filter.keeps(n) && (!searching || n.display_name().to_lowercase().contains(query))) {
        out.truncate(at);
    }
}

/// The rows of the active document as [`list_rows`] lists them, listed again only when the
/// document, its open rows, the search, the filter or Show Layers Only changed.
fn rows_of_doc(ui: &Ui, st: &vectorcraft_engine::DocState, opts: &PanelOptions, query: &str, filter: &Filter) -> Arc<Vec<Row>> {
    let now = RowsKey {
        uid: st.uid,
        revision: st.revision,
        open: st.layers_open.generation(),
        layers_only: opts.layers_only,
        query: query.to_string(),
        filter: filter.clone(),
    };
    let id = key("rows");
    if let Some((was, rows)) = ui.data(|d| d.get_temp::<(RowsKey, Arc<Vec<Row>>)>(id))
        && was == now
    {
        return rows;
    }
    let rows = Arc::new(list_rows(&st.doc, &st.layers_open, opts, query, filter));
    ui.data_mut(|d| d.insert_temp(id, (now, rows.clone())));
    rows
}

/// Everything one frame of the panel reads.
struct View<'a> {
    doc: &'a Arc<Document>,
    /// Where the row thumbnails come from.
    thumbs: ThumbSource<'a>,
    sel: HashSet<NodeId>,
    /// The selected ruler guides.
    guides: Vec<usize>,
    target: Option<NodeId>,
    current: Option<NodeId>,
    rows: Vec<NodeId>,
    opts: PanelOptions,
    t: Tokens,
    h: f32,
    column: Option<ColumnDrag>,
    /// The row to scroll into view.
    reveal: Option<NodeId>,
    /// Preferences › Units › Identify Objects By › XML ID.
    identify_xml_id: bool,
}

/// What one frame of the panel decided.
#[derive(Default)]
struct Out {
    actions: Vec<(String, Value)>,
    /// A row clicked with these modifiers (resolved once every row is listed).
    click: Option<(NodeId, egui::Modifiers)>,
    /// The column drag after this frame (`Some(None)`: it ended).
    column: Option<Option<ColumnDrag>>,
    /// Rows to open or close, and whether to open them.
    toggle: Vec<(NodeId, bool)>,
}

pub fn show(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else { return };
    // A rename belongs to the document it started in (node ids are per document): drop it when
    // another document shows, so it can't rename that document's node with the same id.
    let shown_doc = egui::Id::unique("layers-doc");
    let before = ui.data(|d| d.get_temp::<u64>(shown_doc));
    if before != Some(st.uid) {
        ui.data_mut(|d| {
            if before.is_some() {
                d.remove::<(u64, String)>(rename_id());
            }
            d.insert_temp(shown_doc, st.uid);
        });
    }
    let doc = st.doc.clone();
    let rows = st.highlighted_rows();
    let current = st.current_layer();
    // Rows highlighted anew (a click, Locate Object, a new layer): open the layers around them.
    let seen: Vec<NodeId> = ui.data(|d| d.get_temp(key("seen"))).unwrap_or_default();
    let mut reveal = None;
    if rows != seen {
        open_around(app, &doc, &rows);
        reveal = rows.first().copied();
        ui.data_mut(|d| d.insert_temp(key("seen"), rows.clone()));
    }
    let highlighted = reveal.is_some();
    // A new active layer (a canvas selection highlights no row): reveal it too, unless a row
    // highlighted in the panel holds the view.
    let seen_current: Option<NodeId> = ui.data(|d| d.get_temp(key("seen-current"))).flatten();
    if current != seen_current {
        if rows.is_empty()
            && let Some(layer) = current
        {
            let around = doc.ancestry(layer).unwrap_or_default().split_last().map(|(_, a)| a.to_vec()).unwrap_or_default();
            if let Some(st) = app.session.active_mut() {
                for a in around {
                    st.layers_open.set(a, true);
                }
            }
            reveal = Some(layer);
        }
        ui.data_mut(|d| d.insert_temp(key("seen-current"), current));
    }
    // With Locate Selected Objects on, an object selected anew outside the panel (below).
    let located = newly_selected(app, ui).filter(|_| !highlighted);
    // Search All, and the Filter button at its right.
    let query = ui
        .horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                filter_button(app, ui);
                crate::widgets::search_field(ui, egui::Id::unique("layers-search"), tl!("Search All"))
            })
            .inner
        })
        .inner
        .trim()
        .to_lowercase();
    // The located object, when it has a row once the rows around it are open (Show Layers Only, a
    // search or the filter can leave it out): those rows open, and its row is revealed instead of
    // the row revealed above. Otherwise nothing opens.
    let located = located.filter(|id| {
        app.session.active().is_some_and(|st| listed_when_opened(&doc, &st.layers_open, &app.ui.layers_panel, &query, &app.ui.layers_filter, *id))
    });
    if let Some(id) = located {
        open_around(app, &doc, &[id]);
    }
    let reveal = located.or(reveal);
    let Some(st) = app.session.active() else { return };
    let list = rows_of_doc(ui, st, &app.ui.layers_panel, &query, &app.ui.layers_filter);
    let view = View {
        doc: &doc,
        thumbs: ThumbSource::of(app, st, &doc),
        sel: st.selection.objects.iter().copied().collect(),
        guides: st.selection.guides.clone(),
        target: st.selection.target,
        current: st.current_layer(),
        rows,
        opts: app.ui.layers_panel.clone(),
        t,
        h: app.ui.layers_panel.row(),
        column: ui.data(|d| d.get_temp(key("column"))),
        reveal,
        identify_xml_id: app.session.prefs.identify_objects_by == "xmlId",
    };
    let mut out = Out::default();
    ui.add_space(6.0);
    let list_h = ui.available_height() - 34.0;
    // Only the rows in view are laid out; one revealed out of view is scrolled to.
    let revealed = reveal.and_then(|id| list.iter().position(|r| r.node.id == id));
    let scrolled = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // The bar takes a column of its own, so it never covers the target and selection marks at
        // the rows' right edge (#3443). Whether the rows overflow is known before they are laid
        // out, so the bar is decided up front and the rows never shift when it appears.
        ui.spacing_mut().scroll = crate::widgets::list_scroll_style();
        let overflows = view.h * list.len() as f32 > list_h;
        let bar =
            if overflows { egui::scroll_area::ScrollBarVisibility::AlwaysVisible } else { egui::scroll_area::ScrollBarVisibility::AlwaysHidden };
        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).scroll_bar_visibility(bar).show_rows(
            ui,
            view.h,
            list.len(),
            |ui, range| {
                if let Some(i) = revealed.filter(|i| !range.contains(i)) {
                    let top = ui.max_rect().top() + (i as f32 - range.start as f32) * view.h;
                    ui.scroll_to_rect(egui::Rect::from_min_size(egui::pos2(ui.max_rect().left(), top), vec2(ui.max_rect().width(), view.h)), None);
                }
                for r in list.get(range).unwrap_or_default() {
                    row(ui, &view, r, &mut out);
                }
            },
        )
    });
    let list_rect = scrolled.inner.inner_rect;
    resolve_click(ui, &view, &list, &mut out);
    if let Some(st) = app.session.active_mut() {
        for (id, open) in out.toggle.drain(..) {
            st.layers_open.set(id, open);
        }
    }
    let selected = last_selected(app);
    // The column drag: live while it lasts, one undo step when the button is released.
    let released = ui.input(|i| i.pointer.any_released());
    // Layers dragged into the empty space under the last row go below the bottom layer (there is
    // no row there to take the drop).
    let below = list_rect.top() - scrolled.inner.state.clamped_offset().y + list.len() as f32 * view.h;
    if query.is_empty()
        && !app.ui.layers_filter.is_on()
        && let Some(drag) = egui::DragAndDrop::payload::<LayersDrag>(ui.ctx())
        && let LayersDrag::Rows(ids) = &*drag
        && let Some(bottom) = doc.layers.first()
        && !ids.is_empty()
        && !ids.contains(&bottom.id)
        && ids.iter().all(|i| doc.node(*i).is_some_and(Node::is_layer))
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && list_rect.contains(p)
        && p.y > below
    {
        let stroke = Stroke::new(2.0, view.t.accent);
        ui.painter().line_segment([egui::pos2(list_rect.left(), below), egui::pos2(list_rect.right(), below)], stroke);
        if released {
            let copy = ui.input(|i| i.modifiers.alt);
            let ids: Vec<u64> = ids.iter().map(|i| i.0).collect();
            out.actions.push(("layer.move".into(), json!({"ids": ids, "target": bottom.id.0, "place": "below", "copy": copy})));
        }
    }
    let changed = out.column.is_some();
    let column = out.column.take().unwrap_or_else(|| view.column.clone());
    if let Some(c) = column.as_ref().filter(|_| changed || released) {
        let (prop, label) = if c.eye { ("visible", "Visibility") } else { ("locked", "Lock") };
        let phase = if released { Live::Released } else { Live::Dragging };
        let params = json!({"ids": c.ids, prop: c.value});
        super::live_run(app, label, "layer.setProps", params, phase);
    }
    ui.data_mut(|d| match column.filter(|_| !released) {
        Some(c) => {
            d.insert_temp(key("column"), c);
        }
        None => d.remove::<ColumnDrag>(key("column")),
    });
    if released {
        ui.data_mut(|d| d.remove::<u64>(egui::Id::unique("layers-drag")));
    }
    // Dragged rows show as a dimmed row under the pointer.
    if !released
        && let Some(drag) = egui::DragAndDrop::payload::<LayersDrag>(ui.ctx())
        && let LayersDrag::Rows(ids) = &*drag
        && let Some(src) = ids.first()
        && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
    {
        drag_ghost(ui, &view, *src, pos);
    }
    // Keyboard: Delete removes the highlighted rows while the panel has the keyboard (a row was
    // clicked last, not the canvas).
    let pressed_outside = ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !list_rect.contains(p)));
    if pressed_outside {
        ui.data_mut(|d| d.insert_temp(key("focus"), false));
    }
    let focus: bool = ui.data(|d| d.get_temp(key("focus"))).unwrap_or(false);
    if focus
        && !view.rows.is_empty()
        && ui.ctx().memory(|m| m.focused().is_none())
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace))
    {
        out.actions.push(("layer.delete".into(), json!({})));
    }
    bottom_bar(app, ui, &view, &doc, &mut out.actions);
    for (c, p) in out.actions {
        app.run(&c, p).ok();
    }
    see_selection(app, ui, selected);
}

/// Open the layers and groups around rows `ids`.
fn open_around(app: &mut VectorcraftApp, doc: &Document, ids: &[NodeId]) {
    let around: Vec<NodeId> =
        ids.iter().flat_map(|r| doc.ancestry(*r).unwrap_or_default().split_last().map(|(_, a)| a.to_vec()).unwrap_or_default()).collect();
    if let Some(st) = app.session.active_mut() {
        for a in around {
            st.layers_open.set(a, true);
        }
    }
}

/// Whether [`list_rows`] gives `id` a row once the layers and groups around it are open as well as
/// `open`. Only the rows of `id`'s top-level layer are listed.
fn listed_when_opened(doc: &Document, open: &OpenRows, opts: &PanelOptions, query: &str, filter: &Filter, id: NodeId) -> bool {
    let Some(ancestry) = doc.ancestry(id) else { return false };
    let Some(layer) = doc.layers.iter().find(|l| ancestry.first() == Some(&l.id)) else { return false };
    if doc.mask_edit.is_some_and(|m| m.layer != layer.id) {
        return false;
    }
    let mut open = open.clone();
    for a in ancestry.split_last().map(|(_, a)| a).unwrap_or_default() {
        open.set(*a, true);
    }
    let mut rows = vec![];
    push_rows(layer, 0, false, (Color32::PLACEHOLDER, true), (&open, opts, query, filter, doc), &mut rows);
    rows.iter().any(|r| r.node.id == id && r.guide.is_none())
}

/// The active document's uid and its last selected object, the one Locate Object locates.
fn last_selected(app: &VectorcraftApp) -> Option<(u64, Option<NodeId>)> {
    app.session.active().map(|st| (st.uid, st.selection.objects.last().copied()))
}

/// Locate Selected Objects: the object to locate in this frame. With the option on, that is the
/// last selected object when it differs from the one noted in an earlier frame for the same
/// document. [`see_selection`] notes the panel's own changes, so only a change made on the canvas
/// or by a command is located. This function notes nothing during a drag, so a selection made on
/// the canvas during one is located when the drag ends. When another document becomes active,
/// nothing is located.
fn newly_selected(app: &VectorcraftApp, ui: &Ui) -> Option<NodeId> {
    let busy = app.session.active().is_some_and(|st| st.interaction.is_some()) || app.session.tool_busy();
    let now = last_selected(app).filter(|_| !busy)?;
    let was = ui.data(|d| d.get_temp::<(u64, Option<NodeId>)>(key("selected")));
    ui.data_mut(|d| d.insert_temp(key("selected"), now));
    let anew = was.is_some_and(|w| w.0 == now.0 && w.1 != now.1);
    now.1.filter(|_| anew && app.ui.layers_panel.locate_selected)
}

/// Note the selection as seen when the panel's rows, buttons or menu changed it (`before`: what
/// [`last_selected`] gave before they ran), so Locate Selected Objects does not locate it.
fn see_selection(app: &VectorcraftApp, ui: &Ui, before: Option<(u64, Option<NodeId>)>) {
    if let Some(now) = last_selected(app).filter(|now| Some(*now) != before) {
        ui.data_mut(|d| d.insert_temp(key("selected"), now));
    }
}

/// The Filter button, lit while a filter is on, and its popup: Clear All, the object kinds under
/// By Object and the row states under By Layer, each ticked or not. A tick, or Clear All, runs
/// `ui.layersFilter`; the popup stays open until a click outside it or Escape.
fn filter_button(app: &mut VectorcraftApp, ui: &mut Ui) {
    let f = app.ui.layers_filter.clone();
    let button = widgets::icon_button(ui, "dc-filter", tl!("Filter"), f.is_on(), 24.0);
    let mut set = None;
    egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.horizontal(|ui| {
            widgets::subheader(ui, tl!("Filter"));
            ui.add_space(12.0);
            if ui.add_enabled(f.is_on(), egui::Button::new(egui::RichText::new(tl!("Clear All")).size(12.5)).frame(false)).clicked() {
                set = Some(json!({"objects": [], "layers": []}));
            }
        });
        widgets::subheader(ui, tl!("By Object"));
        for (k, _) in ObjectKind::ALL {
            if widgets::check(ui, k.label(), f.objects.contains(&k), true) {
                set = Some(json!({"objects": toggled(&ObjectKind::ALL, &f.objects, k)}));
            }
        }
        widgets::subheader(ui, tl!("By Layer"));
        for (s, _) in RowState::ALL {
            if widgets::check(ui, s.label(), f.layers.contains(&s), true) {
                set = Some(json!({"layers": toggled(&RowState::ALL, &f.layers, s)}));
            }
        }
    });
    if let Some(p) = set {
        app.run("ui.layersFilter", p).ok();
    }
}

/// The bottom bar: the layer count and the panel's buttons.
fn bottom_bar(app: &mut VectorcraftApp, ui: &mut Ui, view: &View, doc: &Document, actions: &mut Vec<(String, Value)>) {
    let t = &view.t;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0, t.divider));
    let n = doc.layers.len();
    ui.painter().text(
        bar.left_center() + vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        crate::i18n::tn(n as u64, "{n} Layer", "{n} Layers"),
        egui::FontId::proportional(11.5),
        t.text_dim,
    );
    let m = ui.input(|i| i.modifiers);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let trash = widgets::icon_button(&mut child, "trash-2", tl!("Delete Selection"), false, 24.0);
    // A target circle dropped on the trash clears that appearance (dragged rows: below). Peek at
    // the payload's type before taking it: egui's take drops a payload of another type.
    if trash.dnd_hover_payload::<PanelDrag>().is_some()
        && let Some(d) = trash.dnd_release_payload::<PanelDrag>()
        && let PanelDrag::Appearance(id) = *d
    {
        actions.push(("appearance.clear".into(), json!({"ids": [id.0]})));
    } else if trash.clicked() {
        if view.rows.is_empty() && app.session.active().is_some_and(|d| !d.selection.is_empty()) {
            actions.push(("edit.clear".into(), json!({})));
        } else {
            actions.push(("layer.delete".into(), json!({})));
        }
    }
    // Create New Layer: Alt asks for its options first, Ctrl/Cmd puts it on top of every layer.
    let new_layer = widgets::icon_button(&mut child, "file-plus", tl!("Create New Layer"), false, 24.0);
    if new_layer.clicked() {
        if m.alt {
            actions.push(("ui.newLayer".into(), json!({})));
        } else {
            actions.push(("layer.new".into(), json!({"top": m.command})));
        }
    }
    // Rows dropped on the trash are deleted, on New Layer duplicated, as in Illustrator. Both are
    // outlined while rows (or, on the trash, a target circle) hang over them.
    for (button, cmd) in [(&trash, "layer.delete"), (&new_layer, "layer.duplicate")] {
        if let Some(d) = button.dnd_release_payload::<LayersDrag>()
            && let LayersDrag::Rows(ids) = &*d
        {
            actions.push((cmd.into(), json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})));
        }
        let over = button.dnd_hover_payload::<LayersDrag>().is_some_and(|d| matches!(*d, LayersDrag::Rows(_)))
            || (cmd == "layer.delete" && button.dnd_hover_payload::<PanelDrag>().is_some_and(|d| matches!(*d, PanelDrag::Appearance(_))));
        if over {
            child.painter().rect_stroke(button.rect, 3.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
        }
    }
    if widgets::icon_button(&mut child, "plus", tl!("Create New Sublayer"), false, 24.0).clicked() {
        actions.push(if m.alt { ("ui.newLayer".into(), json!({"sublayer": true})) } else { ("layer.newSublayer".into(), json!({})) });
    }
    if widgets::icon_button(&mut child, "frame", tl!("Make/Release Clipping Mask"), false, 24.0).clicked() {
        actions.push(("layer.clippingMask.toggle".into(), json!({})));
    }
    if widgets::icon_button(&mut child, "search", tl!("Locate Object"), false, 24.0).clicked() {
        actions.push(("layer.locate".into(), json!({})));
    }
}

/// Resolve the row clicked this frame: plain (highlight it), Ctrl/Cmd (toggle it) or Shift
/// (highlight the rows from the last one clicked down to it).
fn resolve_click(ui: &Ui, view: &View, list: &[Row], out: &mut Out) {
    let Some((id, m)) = out.click.take() else { return };
    let anchor: Option<NodeId> = ui.data(|d| d.get_temp(key("anchor")));
    ui.data_mut(|d| d.insert_temp(key("focus"), true));
    let at = |id: NodeId| list.iter().position(|r| r.node.id == id);
    if m.shift
        && let Some(a) = anchor.or(view.rows.last().copied())
        && let (Some(i), Some(j)) = (at(a), at(id))
    {
        let range: Vec<u64> = list.get(i.min(j)..=i.max(j)).unwrap_or_default().iter().map(|r| r.node.id.0).collect();
        out.actions.push(("layer.highlight".into(), json!({"ids": range, "mode": if m.command { "add" } else { "set" }})));
        return;
    }
    ui.data_mut(|d| d.insert_temp(key("anchor"), id));
    if m.command {
        out.actions.push(("layer.highlight".into(), json!({"ids": [id.0], "mode": "toggle"})));
    } else {
        out.actions.push(("layer.setCurrent".into(), json!({"id": id.0})));
    }
}

/// Whether row `n` can take `drag` inside it.
fn takes(n: &Node, drag: &LayersDrag, doc: &Document) -> bool {
    match n.kind {
        NodeKind::Layer { .. } => true,
        NodeKind::Group { .. } => match drag {
            LayersDrag::Rows(ids) => !ids.iter().any(|i| doc.node(*i).is_some_and(Node::is_layer)),
            LayersDrag::Art => true,
            LayersDrag::Guides(_) => false,
        },
        _ => false,
    }
}

/// The row of ruler guide `i` on the layer of `item`: a guide mark and its name, the selection
/// square when it is selected. A click selects the guide; Shift or Cmd adds it to (or takes it
/// from) the selected guides. A guide on a hidden or locked layer can't be selected or dragged.
fn guide_row(ui: &mut Ui, view: &View, item: &Row, i: usize, out: &mut Out) {
    let (t, h) = (&view.t, view.h);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
    let Some(g) = view.doc.guides.get(i) else { return };
    let selected = view.guides.contains(&i);
    let editable = view.doc.guide_editable(g);
    if editable && resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.6));
    }
    ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.input_border));
    let mut x = r.left() + 2.0 * COLUMN + 2.0;
    colour_bar(ui, &item.node, r, x, item.colour);
    x += 6.0 + item.depth as f32 * INDENT + 16.0;
    // The guide mark: a short line across or down, in the guides' colour.
    let c = egui::pos2(x + 8.0, r.center().y);
    let half = if g.vertical { vec2(0.0, 6.0) } else { vec2(6.0, 0.0) };
    ui.painter().line_segment([c - half, c + half], Stroke::new(1.5, t.accent));
    x += 22.0;
    let name = format!("<{}>", if g.vertical { tl!("Vertical Guide") } else { tl!("Horizontal Guide") });
    let color = if item.dim { t.text_dim } else { t.text };
    ui.painter().text(egui::pos2(x, r.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), color);
    let col_x = r.right() - 43.5;
    ui.painter().line_segment([egui::pos2(col_x, r.top()), egui::pos2(col_x, r.bottom())], Stroke::new(1.0, t.input_border));
    if selected {
        let q = egui::Rect::from_center_size(egui::pos2(r.right() - SQUARE_X, r.center().y), vec2(7.0, 7.0));
        ui.painter().rect_filled(q, 0.0, item.colour);
        ui.painter().rect_stroke(q, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
    }
    if editable && resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        out.actions.push(("guide.select".into(), json!({"indexes": [i], "toggle": m.shift || m.command})));
    }
    // Dragged: the selected guides when this is one of them, else this one, to another layer's row.
    if editable && resp.drag_started() {
        let guides = if selected { view.guides.clone() } else { vec![i] };
        egui::DragAndDrop::set_payload(ui.ctx(), LayersDrag::Guides(guides));
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    // Guides dropped here go to this guide's layer.
    if let Some(drag) = resp.dnd_hover_payload::<LayersDrag>()
        && let LayersDrag::Guides(guides) = &*drag
    {
        guide_drop(ui, view, guides, item.node.id, r, &resp, out);
    }
}

/// Guide rows `guides` dragged over row `r` (`resp`) of layer `layer`: the row lights up, and the
/// release puts them on that layer (`guide.setLayer`, one undo step).
fn guide_drop(ui: &Ui, view: &View, guides: &[usize], layer: NodeId, r: egui::Rect, resp: &egui::Response, out: &mut Out) {
    ui.painter().rect_stroke(r.shrink(1.0), 0.0, Stroke::new(2.0, view.t.accent), StrokeKind::Inside);
    if resp.dnd_release_payload::<LayersDrag>().is_some() {
        out.actions.push(("guide.setLayer".into(), json!({"indexes": guides, "layer": layer.0})));
    }
}

/// The widgets of row `item`.
fn row(ui: &mut Ui, view: &View, item: &Row, out: &mut Out) {
    if let Some(i) = item.guide {
        return guide_row(ui, view, item, i, out);
    }
    let n = &*item.node;
    let (depth, clip_path, has_children, open, color) = (item.depth, item.clip_path, item.opens, item.open, item.colour);
    let doc = view.doc;
    let t = &view.t;
    let h = view.h;
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
    if view.reveal == Some(n.id) {
        ui.scroll_to_rect(r, None);
    }
    let is_sel = view.sel.contains(&n.id);
    let child_sel = !is_sel
        && n.children().is_some_and(|_| {
            let mut any = false;
            n.walk(&mut |c| any |= c.id != n.id && view.sel.contains(&c.id));
            any
        });
    // Highlighted rows (else the current layer's).
    let highlighted = if view.rows.is_empty() { n.is_layer() && Some(n.id) == view.current } else { view.rows.contains(&n.id) };
    if highlighted {
        ui.painter().rect_filled(r, 0.0, t.row_selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.6));
    }
    ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.input_border));
    if n.is_layer() && Some(n.id) == view.current {
        // Current-layer marker: a small triangle in the top-right corner.
        let c = r.right_top();
        ui.painter().add(egui::Shape::convex_polygon(vec![c, c + vec2(-6.0, 0.0), c + vec2(0.0, 6.0)], t.text, Stroke::NONE));
    }
    eye_and_lock(ui, view, n, r, out);
    // Layer colour bar (every row shows the colour of its layer).
    let mut x = r.left() + 2.0 * COLUMN + 2.0;
    colour_bar(ui, n, r, x, color);
    x += 6.0 + depth as f32 * INDENT;
    // Disclosure triangle; Alt opens or closes everything inside too.
    if has_children {
        let dr = egui::Rect::from_min_size(egui::pos2(x, r.center().y - 8.0), vec2(14.0, 16.0));
        let dresp = ui.interact(dr, ui.scope_id().with(("disc", n.id.0)), Sense::click());
        icons::paint(ui, if open { "chevron-down" } else { "chevron-right" }, dr, t.text_dim);
        if dresp.clicked() {
            out.toggle.push((n.id, !open));
            if ui.input(|i| i.modifiers.alt) {
                n.walk(&mut |c| {
                    if c.id != n.id && opens(c, &view.opts) {
                        out.toggle.push((c.id, !open));
                    }
                });
            }
        }
    }
    x += 16.0;
    // Thumbnail.
    let thumb_rect = view.opts.thumb(n).then(|| {
        let th = row_thumb(ui, view, &item.node, r, x);
        x = th.right() + 4.0;
        th
    });
    // Name (Preferences › Units › Identify Objects By: Object Name or XML ID).
    let xml_id = view.identify_xml_id;
    let name = if doc.mask_edit.is_some_and(|m| m.layer == n.id) { "<Opacity Mask>".to_string() } else { n.identify_label(xml_id) };
    let font = egui::FontId::proportional(13.0);
    let renaming: Option<(u64, String)> = ui.data(|d| d.get_temp(rename_id()));
    let name_rect = egui::Rect::from_min_max(egui::pos2(x - 2.0, r.center().y - 10.0), egui::pos2(r.right() - 44.0, r.center().y + 10.0));
    let name_end;
    match renaming {
        Some((rid, mut buf)) if rid == n.id.0 => {
            let field = rename_id().with("field");
            let kept = crate::widgets::own_field_shift_enter(ui, field, &mut buf, &name);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(name_rect));
            let te = child.add(egui::TextEdit::singleline(&mut buf).id(field).desired_width(name_rect.width()).font(font.clone()));
            // Focus the field as it opens. Asking every frame took the focus back from Enter,
            // Escape or a click elsewhere, so the rename never ended.
            if !te.has_focus() && !te.lost_focus() {
                te.request_focus();
            }
            if te.lost_focus() {
                ui.data_mut(|d| d.remove::<(u64, String)>(rename_id()));
                if child.input(|i| !i.key_pressed(egui::Key::Escape)) && buf != name {
                    out.actions.push(("layer.setProps".into(), json!({"ids": [n.id.0], "name": buf})));
                }
            } else {
                // Shift+Enter renames it and keeps the field open.
                if kept && buf != name {
                    out.actions.push(("layer.setProps".into(), json!({"ids": [n.id.0], "name": buf.clone()})));
                }
                ui.data_mut(|d| d.insert_temp(rename_id(), (n.id.0, buf)));
            }
            name_end = name_rect.right();
        }
        _ => {
            let painter = ui.painter().with_clip_rect(name_rect);
            // Generated names ("<Path>", "<Opacity Mask>") are translated where painted; the stored name stays English.
            // XML IDs stay as ids (not translated).
            let shown = if doc.mask_edit.is_some_and(|m| m.layer == n.id) {
                tl!("<Opacity Mask>").to_string()
            } else if xml_id {
                name.clone()
            } else {
                painted_name(n, &n.display_name(), crate::i18n::current())
            };
            let text = painter.text(egui::pos2(x, r.center().y), egui::Align2::LEFT_CENTER, shown, font, if item.dim { t.text_dim } else { t.text });
            name_end = text.right();
            // A clipping path's name is underlined, a masked object's with a dashed line.
            if clip_path {
                painter.line_segment([text.left_bottom(), text.right_bottom()], Stroke::new(1.0, t.text));
            } else if n.extras().mask.is_some() {
                painter.extend(egui::Shape::dashed_line(&[text.left_bottom(), text.right_bottom()], Stroke::new(1.0, t.text), 3.0, 2.0));
            }
        }
    }
    // Target circle and selection square.
    let col_x = r.right() - 43.5;
    ui.painter().line_segment([egui::pos2(col_x, r.top()), egui::pos2(col_x, r.bottom())], Stroke::new(1.0, t.input_border));
    let tc = egui::pos2(r.right() - TARGET_X, r.center().y);
    let tresp = target_circle(ui, n, tc, h, view.target.map_or(is_sel, |id| id == n.id), &mut out.actions, t);
    let sq = egui::pos2(r.right() - SQUARE_X, r.center().y);
    if is_sel {
        let q = egui::Rect::from_center_size(sq, vec2(7.0, 7.0));
        ui.painter().rect_filled(q, 0.0, color);
        ui.painter().rect_stroke(q, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
    } else if child_sel {
        ui.painter().rect_filled(egui::Rect::from_center_size(sq, vec2(4.0, 4.0)), 0.0, color);
    }
    let sq_resp = ui.interact(egui::Rect::from_center_size(sq, vec2(18.0, h)), ui.scope_id().with(("selsq", n.id.0)), Sense::click_and_drag());
    // Dragging the selected-art square carries the selection to another row.
    if (is_sel || child_sel) && sq_resp.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), LayersDrag::Art);
    }
    let m = ui.input(|i| i.modifiers);
    if tresp.clicked() {
        out.actions.push(("layer.target".into(), json!({"id": n.id.0, "add": m.shift})));
    } else if sq_resp.clicked() {
        out.actions.push(("layer.selectAll".into(), json!({"id": n.id.0, "add": m.shift})));
    } else if resp.clicked() {
        out.click = Some((n.id, m));
    }
    if resp.double_clicked() {
        let on_name = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| p.x <= name_end.max(name_rect.left() + 20.0) && p.x >= name_rect.left());
        let on_thumb = thumb_rect.is_some_and(|th| ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| th.contains(p)));
        if on_name && !on_thumb {
            ui.data_mut(|d| d.insert_temp(rename_id(), (n.id.0, n.identify_label(view.identify_xml_id))));
        } else {
            out.actions.push(("ui.layerOptions".into(), json!({"ids": [n.id.0]})));
        }
    }
    // Row menu: Collect for Export on this row (or every highlighted row when this is one of them).
    let collect_ids: Vec<u64> = if view.rows.contains(&n.id) { view.rows.iter().map(|id| id.0).collect() } else { vec![n.id.0] };
    resp.context_menu(|ui| {
        collect_for_export_menu(ui, true, |multiple| {
            out.actions.push(("assets.add".into(), json!({ "ids": collect_ids.clone(), "multiple": multiple })));
        });
    });
    // Dragging rows: the highlighted rows when this is one of them, else this row.
    if resp.drag_started() && view.column.is_none() {
        let ids = if view.rows.contains(&n.id) { view.rows.clone() } else { vec![n.id] };
        egui::DragAndDrop::set_payload(ui.ctx(), LayersDrag::Rows(ids));
        let grab = resp.interact_pointer_pos().map_or(vec2(h, h / 2.0), |p| p - r.min);
        ui.data_mut(|d| d.insert_temp(key("grab"), (grab, r.width(), depth)));
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    drop_target(ui, view, n, r, x - 16.0, open, &resp, out);
}

/// Alt-clicking row `n`'s eye (`eye`) or lock: Hide Others or Lock Others, every top-level layer
/// but the one holding the row; when they all are hidden (locked) already, Show All Layers (Unlock
/// All Layers). Each is one undo step.
fn others_action(doc: &Document, n: &Node, eye: bool) -> (String, Value) {
    let own = doc.layer_of(n.id);
    let mut others = doc.layers.iter().filter(|l| Some(l.id) != own);
    let cmd = match (eye, if eye { others.any(|l| l.visible) } else { others.any(|l| !l.locked) }) {
        (true, true) => "layer.hideOthers",
        (true, false) => "layer.showAll",
        (false, true) => "layer.lockOthers",
        (false, false) => "layer.unlockAll",
    };
    // Show All and Unlock All take no rows.
    let params = if cmd.ends_with("Others") { json!({ "ids": [n.id.0] }) } else { json!({}) };
    (cmd.into(), params)
}

/// Ctrl/Cmd+Alt-clicking row `n`'s eye runs Outline Others, which puts every top-level layer but the
/// one holding the row in Outline and puts that one in Preview. When all the other layers are in
/// Outline already, it runs Preview All Layers. Each is one undo step.
fn outline_others_action(doc: &Document, n: &Node) -> (String, Value) {
    let own = doc.layer_of(n.id);
    if doc.layers.iter().any(|l| Some(l.id) != own && !matches!(l.kind, NodeKind::Layer { preview: false, .. })) {
        ("layer.outlineOthers".into(), json!({ "ids": [n.id.0] }))
    } else {
        // Preview All takes no rows.
        ("layer.previewAll".into(), json!({}))
    }
}

/// The eye and lock columns of row `n` at `r`.
fn eye_and_lock(ui: &mut Ui, view: &View, n: &Node, r: egui::Rect, out: &mut Out) {
    let t = &view.t;
    let h = view.h;
    let eye = egui::Rect::from_min_size(r.min, vec2(COLUMN, h));
    let lock = egui::Rect::from_min_size(r.min + vec2(COLUMN, 0.0), vec2(COLUMN, h));
    ui.painter().line_segment([eye.right_top(), eye.right_bottom()], Stroke::new(1.0, t.input_border));
    ui.painter().line_segment([lock.right_top(), lock.right_bottom()], Stroke::new(1.0, t.input_border));
    let preview = !matches!(n.kind, NodeKind::Layer { preview: false, .. });
    let er = ui.interact(eye, ui.scope_id().with(("eye", n.id.0)), Sense::click_and_drag());
    if n.visible {
        let icon = egui::Rect::from_center_size(eye.center(), vec2(14.0, 14.0));
        if n.is_template() {
            // A template layer shows the template mark instead of the eye.
            icons::paint(ui, "shapes", icon, t.icon);
        } else if preview {
            icons::paint(ui, "eye", icon, t.icon);
        } else {
            // Outline view: a hollow eye.
            icons::paint(ui, "eye", icon, t.text_dim);
            ui.painter().circle_stroke(eye.center(), 7.5, Stroke::new(1.0, t.text_dim));
        }
    }
    let lr = ui.interact(lock, ui.scope_id().with(("lock", n.id.0)), Sense::click_and_drag());
    if n.locked {
        icons::paint(ui, "lock", egui::Rect::from_center_size(lock.center(), vec2(12.0, 12.0)), t.icon);
    } else if lr.hovered() {
        icons::paint(ui, "lock", egui::Rect::from_center_size(lock.center(), vec2(12.0, 12.0)), t.text_disabled);
    }
    let m = ui.input(|i| i.modifiers);
    if er.clicked() {
        if m.alt && m.command {
            out.actions.push(outline_others_action(view.doc, n));
        } else if m.alt {
            out.actions.push(others_action(view.doc, n, true));
        } else if m.command && n.is_layer() {
            // Ctrl/Cmd-click: Preview ↔ Outline for this layer.
            out.actions.push(("layer.setProps".into(), json!({"ids": [n.id.0], "preview": !preview})));
        } else {
            out.actions.push(("layer.setProps".into(), json!({"ids": [n.id.0], "visible": !n.visible})));
        }
    }
    if lr.clicked() && m.alt {
        out.actions.push(others_action(view.doc, n, false));
    } else if lr.clicked() {
        out.actions.push(("layer.setProps".into(), json!({"ids": [n.id.0], "locked": !n.locked})));
    }
    // Dragging down a column gives every row it passes this row's new state.
    if er.drag_started() {
        out.column = Some(Some(ColumnDrag { eye: true, value: !n.visible, ids: vec![n.id.0] }));
    } else if lr.drag_started() {
        out.column = Some(Some(ColumnDrag { eye: false, value: !n.locked, ids: vec![n.id.0] }));
    } else if let Some(c) = out.column.clone().unwrap_or_else(|| view.column.clone())
        && !c.ids.contains(&n.id.0)
        && ui.input(|i| i.pointer.primary_down() && i.pointer.interact_pos().is_some_and(|p| p.y >= r.top() && p.y < r.bottom()))
    {
        let mut c = c;
        c.ids.push(n.id.0);
        out.column = Some(Some(c));
    }
}

/// Row `n` at `r` as a drop target for dragged rows or the selected-art square: shows where they
/// would go (a line above or below it, or a box around it for inside) and moves them on release
/// (Alt copies). `indent` is where the row's content starts.
#[allow(clippy::too_many_arguments)]
fn drop_target(ui: &Ui, view: &View, n: &Node, r: egui::Rect, indent: f32, open: bool, resp: &egui::Response, out: &mut Out) {
    let Some(drag) = resp.dnd_hover_payload::<LayersDrag>() else { return };
    let doc = view.doc;
    let moving: Vec<NodeId> = match &*drag {
        LayersDrag::Rows(ids) => ids.clone(),
        LayersDrag::Art => view.sel.iter().copied().collect(),
        LayersDrag::Guides(guides) => {
            // Guides go to the layer of the row (the row's own, for a layer).
            if let Some(layer) = if n.is_layer() { Some(n.id) } else { doc.layer_containing(n.id) } {
                guide_drop(ui, view, guides, layer, r, resp, out);
            }
            return;
        }
    };
    // Never into itself or a row inside it.
    let around = doc.ancestry(n.id).unwrap_or_default();
    if moving.is_empty() || moving.iter().any(|m| around.contains(m)) {
        return;
    }
    let Some(p) = ui.input(|i| i.pointer.hover_pos()) else { return };
    let rel = (p.y - r.top()) / r.height().max(1.0);
    let inside = takes(n, &drag, doc);
    let place = match &*drag {
        LayersDrag::Art if inside => Place::Inside,
        LayersDrag::Art => Place::Above,
        LayersDrag::Rows(_) if inside && rel >= 0.25 && (rel <= 0.75 || open) => Place::Inside,
        LayersDrag::Rows(_) if rel < 0.5 => Place::Above,
        LayersDrag::Rows(_) | LayersDrag::Guides(_) => Place::Below,
    };
    let stroke = Stroke::new(2.0, view.t.accent);
    match place {
        Place::Above => {
            ui.painter().line_segment([egui::pos2(indent, r.top()), egui::pos2(r.right(), r.top())], stroke);
        }
        Place::Below => {
            ui.painter().line_segment([egui::pos2(indent, r.bottom()), egui::pos2(r.right(), r.bottom())], stroke);
        }
        Place::Inside => {
            ui.painter().rect_stroke(r.shrink(1.0), 0.0, stroke, StrokeKind::Inside);
        }
    }
    if resp.dnd_release_payload::<LayersDrag>().is_some() {
        let copy = ui.input(|i| i.modifiers.alt);
        let ids: Vec<u64> = match &*drag {
            // The selection's top objects, as Arrange and Group take them.
            LayersDrag::Art => doc.paint_order(moving.iter().copied()).iter().map(|i| i.0).collect(),
            LayersDrag::Rows(ids) => ids.iter().map(|i| i.0).collect(),
            LayersDrag::Guides(_) => return,
        };
        out.actions.push(("layer.move".into(), json!({"ids": ids, "target": n.id.0, "place": place.id(), "copy": copy})));
    }
}

/// The target circle of `n`'s row at `c`: a ring, doubled while `n` is targeted, filled when `n`
/// has an appearance or transparency of its own. Its response is clicked to target `n`; dragging
/// it carries `n`'s appearance ([`PanelDrag::Appearance`]) onto another row's circle (Alt
/// copies it) or the trash.
fn target_circle(ui: &mut Ui, n: &Node, c: egui::Pos2, h: f32, targeted: bool, actions: &mut Vec<(String, Value)>, t: &Tokens) -> egui::Response {
    let resp = ui.interact(egui::Rect::from_center_size(c, vec2(16.0, h)), ui.scope_id().with(("target", n.id.0)), Sense::click_and_drag());
    widgets::drag_source(ui, &resp, || PanelDrag::Appearance(n.id));
    let held = resp.dnd_hover_payload::<PanelDrag>().is_some_and(|d| matches!(*d, PanelDrag::Appearance(id) if id != n.id));
    if let Some(d) = resp.dnd_release_payload::<PanelDrag>()
        && let PanelDrag::Appearance(source) = *d
        && source != n.id
    {
        let copy = ui.input(|i| i.modifiers.alt);
        actions.push(("appearance.transfer".into(), json!({"source": source.0, "target": n.id.0, "copy": copy})));
    }
    let ring = if held { t.accent } else { t.icon };
    ui.painter().circle_stroke(c, 5.0, Stroke::new(if held { 1.5 } else { 1.0 }, ring));
    if targeted {
        ui.painter().circle_stroke(c, 2.8, Stroke::new(1.0, t.icon));
    }
    if has_styled_target(n) {
        ui.painter().circle_filled(c, 3.2, t.icon);
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    resp
}

/// Whether an object's target circle is filled: its appearance is not basic (a group or layer:
/// it has fills, strokes or effects of its own) or its transparency (opacity, blend mode,
/// isolation, knockout or an opacity mask) is not the default.
fn has_styled_target(n: &Node) -> bool {
    !n.appearance.is_basic()
        || (matches!(n.kind, NodeKind::Group { .. } | NodeKind::Layer { .. }) && !n.appearance.items.is_empty())
        || !n.has_default_transparency()
}

/// Where row thumbnails come from: document `uid` at `revision`, and whether a drag is in
/// progress (the thumbnails shown then stay until it ends).
pub(crate) struct ThumbSource<'a> {
    pub uid: u64,
    pub revision: u64,
    pub doc: &'a Arc<Document>,
    pub busy: bool,
}

impl<'a> ThumbSource<'a> {
    /// The thumbnails of the document `st` holds (`doc`, its document).
    pub(crate) fn of(app: &VectorcraftApp, st: &vectorcraft_engine::DocState, doc: &'a Arc<Document>) -> Self {
        Self { uid: st.uid, revision: st.revision, doc, busy: st.interaction.is_some() || app.session.tool_busy() }
    }
}

/// Node `id` of `doc`, as the `Arc` its parent holds.
pub(crate) fn node_arc(doc: &Document, id: NodeId) -> Option<Arc<Node>> {
    let path = doc.index_path(id)?;
    let (first, rest) = path.split_first()?;
    let mut cur = doc.layers.get(*first)?;
    for &i in rest {
        cur = cur.children()?.get(i)?;
    }
    Some(cur.clone())
}

/// A row thumbnail's image.
struct ThumbEntry {
    /// The node it shows. Weak, so the cache doesn't keep old art alive, but its allocation stays
    /// and no other node can come to have its address.
    node: Weak<Node>,
    px: u32,
    /// None: the node draws nothing.
    tex: Option<egui::TextureHandle>,
    /// How long it took to render (ms).
    ms: f64,
    /// The pass it was last drawn in.
    used: u64,
}

/// A row thumbnail job: the row (document uid, node), the node and the size rendered.
type ThumbJob = ((u64, NodeId), Weak<Node>, u32);

#[derive(Default)]
struct Thumbs {
    entries: HashMap<(u64, NodeId), ThumbEntry>,
    /// What the worker is rendering, by row.
    pending: HashMap<(u64, NodeId), (Weak<Node>, u32)>,
    worker: Option<ThumbWorker<(u64, NodeId), ThumbJob>>,
    worker_started: bool,
    settle: Settle,
    /// (pass, ms, count) rendered on the UI thread in that pass.
    spent: (u64, f64, u32),
    /// Thumbnails rendered on the UI thread (tests count them).
    renders: u64,
}

thread_local! {
    static THUMBS: crate::graphics::TexCache<Thumbs> = crate::graphics::TexCache::default();
}

/// The most row thumbnails kept: past it the ones drawn longest ago go.
const MAX_THUMBS: usize = 512;
/// Rendering a thumbnail that took longer than this (ms) again goes to the worker.
const LIGHT_THUMB_MS: f64 = 4.0;
/// A node with more objects than this inside has its first thumbnail made on the worker.
const LIGHT_THUMB_NODES: usize = 200;
/// At most this long (ms) and this many thumbnails are rendered on the UI thread in a pass; the
/// rest go to the worker, or wait for the next pass where there is none.
const THUMB_BUDGET_MS: f64 = 8.0;
const THUMB_BUDGET_COUNT: u32 = 16;

/// How row thumbnails render: images far smaller than they are sample their mip levels.
pub(crate) fn thumb_options() -> vectorcraft_render::RenderOptions {
    vectorcraft_render::RenderOptions { image_mips: true, ..Default::default() }
}

/// Has `n` at most `limit` objects inside it (counting stops there)?
fn small_node(n: &Node, limit: usize) -> bool {
    fn count(n: &Node, left: &mut usize) -> bool {
        for c in n.children().into_iter().flatten() {
            if *left == 0 {
                return false;
            }
            *left -= 1;
            if !count(c, left) {
                return false;
            }
        }
        true
    }
    let mut left = limit;
    count(n, &mut left)
}

impl Thumbs {
    /// Keep `img` (None: nothing drawn) as the thumbnail of row `key` showing `node`.
    fn upload(&mut self, ctx: &egui::Context, key: (u64, NodeId), node: Weak<Node>, px: u32, img: Option<&vectorcraft_render::Rendered>, ms: f64) {
        let color = img.map(crate::canvas::color_image);
        let used = ctx.cumulative_pass_nr();
        let name = format!("layer-thumb-{}", key.1.0);
        match self.entries.get_mut(&key) {
            Some(e) => {
                match (color, &mut e.tex) {
                    (Some(color), Some(tex)) => tex.set(color, egui::TextureOptions::LINEAR),
                    (Some(color), tex) => *tex = Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR)),
                    (None, tex) => *tex = None,
                }
                (e.node, e.px, e.ms, e.used) = (node, px, ms, used);
            }
            None => {
                let tex = color.map(|color| ctx.load_texture(name, color, egui::TextureOptions::LINEAR));
                self.entries.insert(key, ThumbEntry { node, px, tex, ms, used });
                if self.entries.len() > MAX_THUMBS {
                    // Keep the three quarters drawn last.
                    let mut by_use: Vec<u64> = self.entries.values().map(|e| e.used).collect();
                    by_use.sort_unstable();
                    let cut = by_use.get(by_use.len() - MAX_THUMBS * 3 / 4).copied().unwrap_or(0);
                    self.entries.retain(|_, e| e.used >= cut);
                }
            }
        }
    }

    /// The texture of row `n` (`px` pixels square) to draw: made or updated now, or on the
    /// worker (the old one is drawn meanwhile). Thumbnails shown stay during a drag and until
    /// the document settles.
    fn texture(&mut self, ctx: &egui::Context, src: &ThumbSource, n: &Arc<Node>, px: u32) -> Option<egui::TextureHandle> {
        if !self.worker_started {
            self.worker_started = true;
            self.worker = ThumbWorker::spawn(ctx, "thumbnails");
        }
        let done = match self.worker.as_ref().map(ThumbWorker::poll) {
            Some(Some(done)) => done,
            Some(None) => {
                // The thread is gone: render here from now on.
                self.worker = None;
                self.pending.clear();
                vec![]
            }
            None => vec![],
        };
        for d in done {
            let (key, node, px) = d.key;
            if self.pending.get(&key).is_some_and(|(p, ppx)| p.ptr_eq(&node) && *ppx == px) {
                self.pending.remove(&key);
            }
            self.upload(ctx, key, node, px, d.img.as_ref(), d.ms);
        }
        let key = (src.uid, n.id);
        let pass = ctx.cumulative_pass_nr();
        let (fresh, light) = match self.entries.get_mut(&key) {
            Some(e) => {
                e.used = pass;
                (std::ptr::eq(e.node.as_ptr(), Arc::as_ptr(n)) && e.px == px, Some(e.ms <= LIGHT_THUMB_MS))
            }
            None => (false, None),
        };
        let shown = self.entries.get(&key).and_then(|e| e.tex.clone());
        if fresh || self.pending.get(&key).is_some_and(|(p, ppx)| std::ptr::eq(p.as_ptr(), Arc::as_ptr(n)) && *ppx == px) {
            return shown;
        }
        if light.is_some() && (src.busy || !self.settle.settled(ctx, src.uid, src.revision, ctx.input(|i| i.time))) {
            return shown;
        }
        if self.spent.0 != pass {
            self.spent = (pass, 0.0, 0);
        }
        let in_budget = self.spent.1 < THUMB_BUDGET_MS && self.spent.2 < THUMB_BUDGET_COUNT;
        let light = light.unwrap_or_else(|| small_node(n, LIGHT_THUMB_NODES)) && in_budget;
        if !light && let Some(w) = &self.worker {
            let (doc, node) = (src.doc.clone(), n.clone());
            let job: ThumbJob = (key, Arc::downgrade(n), px);
            if w.submit(key, job, Box::new(move |r| r.render_node_thumbnail_with(&doc, &node, px, None, &thumb_options()))) {
                self.pending.insert(key, (Arc::downgrade(n), px));
                return shown;
            }
            self.worker = None;
            self.pending.clear();
        }
        if !in_budget {
            ctx.request_repaint();
            return shown;
        }
        thread_local! {
            static RENDERER: std::cell::RefCell<vectorcraft_render::Renderer> = std::cell::RefCell::new(vectorcraft_render::Renderer::new());
        }
        let t0 = crate::now_ms();
        let img = RENDERER.with(|r| r.borrow_mut().render_node_thumbnail_with(src.doc, n, px, None, &thumb_options()));
        let ms = crate::now_ms() - t0;
        self.spent.1 += ms;
        self.spent.2 += 1;
        self.renders += 1;
        self.upload(ctx, key, Arc::downgrade(n), px, img.as_ref(), ms);
        self.entries.get(&key).and_then(|e| e.tex.clone())
    }
}

/// A real rendered thumbnail of row `n`, cached by node identity (unchanged nodes keep their `Arc`
/// allocation, so the address is a free change detector). Only rendered for visible rows; see
/// [`Thumbs::texture`] for when.
pub(crate) fn real_thumb(ui: &Ui, src: &ThumbSource, n: &Arc<Node>, r: egui::Rect) -> bool {
    if !ui.is_rect_visible(r) {
        return true;
    }
    let ppp = ui.ctx().pixels_per_point();
    let px = ((r.width() * ppp).round() as u32).max(8);
    match THUMBS.with(|c| c.borrow_mut().texture(ui.ctx(), src, n, px)) {
        Some(t) => {
            ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            true
        }
        None => false,
    }
}

/// Tiny vector thumbnail painted with egui (fallback when rendering isn't possible).
fn thumb(ui: &Ui, n: &Node, r: egui::Rect) {
    let Some(b) = n.visual_bounds() else { return };
    let s = ((r.width() - 3.0) as f64 / b.width().max(b.height()).max(1e-6)) as f32;
    let off = r.center() - vec2(b.center().x as f32 * s, b.center().y as f32 * s);
    let p = ui.painter().with_clip_rect(r);
    let mut count = 0;
    n.walk(&mut |c| {
        if count > 40 {
            return;
        }
        if let Some(pd) = c.path_data() {
            count += 1;
            let col = c.appearance.fill_paint().color().or(c.appearance.stroke_paint().color()).map(|cc| {
                let [a, b2, d, _] = cc.to_rgba8(1.0);
                Color32::from_rgb(a, b2, d)
            });
            if let Some(bb) = pd.bounds() {
                let rr = egui::Rect::from_min_max(off + vec2(bb.x0 as f32 * s, bb.y0 as f32 * s), off + vec2(bb.x1 as f32 * s, bb.y1 as f32 * s));
                p.rect_filled(rr, 0.0, col.unwrap_or(Color32::from_gray(120)));
            }
        }
    });
}

/// A panel menu item: label, command, params, enabled, checked.
type MenuItem<'a> = (&'a str, &'a str, Value, bool, bool);

/// The Layers panel's (≡) menu.
pub fn menu(app: &mut VectorcraftApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.clone();
    let rows = st.highlighted_rows();
    let current = st.current_layer();
    let has_sel = !st.selection.is_empty();
    let isolated = st.isolation.is_some();
    let targets: Vec<NodeId> = if rows.is_empty() { current.into_iter().collect() } else { rows.clone() };
    let one_container = match targets.as_slice() {
        [id] => doc.node(*id).is_some_and(|n| matches!(n.kind, NodeKind::Layer { .. } | NodeKind::Group { .. })),
        _ => false,
    };
    let layers_in: Vec<&Node> = targets.iter().filter_map(|id| doc.node(*id)).filter(|n| n.is_layer()).collect();
    let template = layers_in.first().is_some_and(|l| l.is_template());
    // The top-level layers holding the rows, and the others.
    let kept: HashSet<NodeId> = targets.iter().filter_map(|id| doc.layer_of(*id)).collect();
    let others: Vec<&Node> = doc.layers.iter().map(|l| &**l).filter(|l| !kept.contains(&l.id)).collect();
    let mut all_layers = vec![];
    doc.walk(|n| {
        if n.is_layer() {
            all_layers.push(n);
        }
    });
    let mut items: Vec<Option<MenuItem>> = vec![
        Some(("New Layer…", "ui.newLayer", json!({}), true, false)),
        Some(("New Sublayer…", "ui.newLayer", json!({"sublayer": true}), current.is_some(), false)),
        Some(("Duplicate Selection", "layer.duplicate", json!({}), !targets.is_empty(), false)),
        Some(("Delete Selection", "layer.delete", json!({}), !targets.is_empty(), false)),
        Some(("Options for Selection…", "ui.layerOptions", json!({}), !targets.is_empty(), false)),
        None,
        Some(("Make/Release Clipping Mask", "layer.clippingMask.toggle", json!({}), true, false)),
        None,
    ];
    items.push(if isolated {
        Some(("Exit Isolation Mode", "object.exitIsolation", json!({}), true, false))
    } else {
        let id = targets.first().map(|i| i.0);
        Some(("Enter Isolation Mode", "object.isolate", json!({ "id": id }), one_container, false))
    });
    items.extend([
        None,
        Some(("Locate Object", "layer.locate", json!({}), has_sel, false)),
        // Sentinel: rendered as the Save Selection submenu (the Select menu's saved-selection items).
        Some(("Save Selection", "", json!({}), true, false)),
        None,
        Some(("Merge Selected", "layer.merge", json!({}), rows.len() >= 2, false)),
        Some(("Flatten Artwork", "layer.flatten", json!({}), true, false)),
        Some(("Collect in New Layer", "layer.collectInNew", json!({}), !rows.is_empty() || has_sel, false)),
        // Sentinel: rendered as Collect for Export › As Single / As Multiple (assets.add).
        Some(("Collect for Export", "", json!({}), !targets.is_empty() || has_sel, false)),
        Some(("Release to Layers (Sequence)", "layer.releaseToLayers", json!({}), one_container, false)),
        Some(("Release to Layers (Build)", "layer.releaseToLayersBuild", json!({}), one_container, false)),
        Some(("Reverse Order", "layer.reverse", json!({}), rows.len() >= 2, false)),
        None,
        Some(("Template", "layer.template", json!({}), !layers_in.is_empty(), template)),
    ]);
    // Each pair reads by state: Hide Others while another layer shows, else Show All Layers.
    let any_other = |f: &dyn Fn(&Node) -> bool| others.iter().any(|l| f(l));
    items.push(Some(if any_other(&|l| l.visible) {
        ("Hide Others", "layer.hideOthers", json!({}), true, false)
    } else {
        ("Show All Layers", "layer.showAll", json!({}), all_layers.iter().any(|l| !l.visible), false)
    }));
    items.push(Some(if any_other(&|l| !matches!(l.kind, NodeKind::Layer { preview: false, .. })) {
        ("Outline Others", "layer.outlineOthers", json!({}), true, false)
    } else {
        (
            "Preview All Layers",
            "layer.previewAll",
            json!({}),
            all_layers.iter().any(|l| matches!(l.kind, NodeKind::Layer { preview: false, .. })),
            false,
        )
    }));
    items.push(Some(if any_other(&|l| !l.locked) {
        ("Lock Others", "layer.lockOthers", json!({}), true, false)
    } else {
        ("Unlock All Layers", "layer.unlockAll", json!({}), all_layers.iter().any(|l| l.locked), false)
    }));
    items.push(None);
    let remembers = doc.paste_remembers_layers;
    items.push(Some(("Paste Remembers Layers", "layer.pasteRemembersLayers", json!({"on": !remembers}), true, remembers)));
    items.push(None);
    items.push(Some(("Panel Options…", "ui.layersPanelOptions", json!({}), true, false)));
    for item in items {
        match item {
            None => {
                ui.separator();
            }
            Some(("Collect for Export", _, _, enabled, _)) => {
                collect_for_export_menu(ui, enabled, |multiple| {
                    let mut p = json!({ "multiple": multiple });
                    if !targets.is_empty() {
                        p["ids"] = json!(targets.iter().map(|id| id.0).collect::<Vec<_>>());
                    }
                    app.run("assets.add", p).ok();
                });
            }
            Some(("Save Selection", "", ..)) => save_selection_menu(app, ui),
            Some((label, cmd, p, enabled, checked)) => {
                if widgets::menu_item(ui, label, enabled, checked) {
                    let selected = last_selected(app);
                    app.run(cmd, p).ok();
                    see_selection(app, ui, selected);
                }
            }
        }
    }
}

/// Collect for Export › As Single Asset / As Multiple Assets (`assets.add`).
fn collect_for_export_menu(ui: &mut Ui, enabled: bool, mut run: impl FnMut(bool)) {
    ui.add_enabled_ui(enabled, |ui| {
        // English labels match the rest of this panel menu (and Object › Collect for Export).
        ui.menu_button("Collect for Export", |ui| {
            if widgets::menu_item(ui, "As Single Asset", true, false) {
                run(false);
                ui.close();
            }
            if widgets::menu_item(ui, "As Multiple Assets", true, false) {
                run(true);
                ui.close();
            }
        });
    });
}

/// Save Selection › Save Selection…, Update Selection and Edit Selection…, enabled and run as in
/// the Select menu, then the active document's saved selections: choosing one recalls it.
fn save_selection_menu(app: &mut VectorcraftApp, ui: &mut Ui) {
    ui.menu_button(format!("   {}", tl!("Save Selection")), |ui| {
        for (label, id) in [("Save Selection…", "select.save"), ("Update Selection", "select.update"), ("Edit Selection…", "select.editSaved")] {
            if widgets::menu_item(ui, label, crate::menus::enabled(app, id), false) {
                crate::menus::invoke(app, id, json!({}));
                ui.close();
            }
        }
        let names: Vec<String> = app.session.active().map(|d| d.doc.saved_selections.iter().map(|s| s.name.clone()).collect()).unwrap_or_default();
        if !names.is_empty() {
            ui.separator();
        }
        for name in names {
            if widgets::menu_item_name(ui, &name, true, false) {
                let selected = last_selected(app);
                app.run("select.recall", json!({ "name": name })).ok();
                see_selection(app, ui, selected);
                ui.close();
            }
        }
    });
}

/// `ui.layersExpand {ids?, open?}`: open (or close) rows of the active document's Layers panel;
/// without `ids`, every row that holds others.
pub fn expand(app: &mut VectorcraftApp, p: &Value) -> Result<Value, String> {
    let st = app.session.active().ok_or("no document open")?;
    let open = p.get("open").and_then(Value::as_bool).unwrap_or(true);
    let ids: Vec<u64> = match p.get("ids").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).filter(|i| st.doc.node(NodeId(*i)).is_some()).collect(),
        None => {
            let mut v = vec![];
            st.doc.walk(|n| {
                if n.is_container() {
                    v.push(n.id.0);
                }
            });
            v
        }
    };
    let st = app.session.active_mut().ok_or("no document open")?;
    for i in &ids {
        st.layers_open.set(NodeId(*i), open);
    }
    Ok(json!({ "count": ids.len() }))
}

/// `ui.layersFilter {objects?, layers?}`: set the Layers panel's Filter, `objects` to the object
/// kinds ticked under By Object and `layers` to the row states ticked under By Layer, by parameter
/// name ([`ObjectKind::ALL`], [`RowState::ALL`]). A key left out or set to null keeps its list; any
/// other value that is not an array of those names is an error, and then nothing changes.
/// → {objects, layers}
pub fn set_filter(app: &mut VectorcraftApp, p: &Value) -> Result<Value, String> {
    let objects = parse_names(p, "objects", &ObjectKind::ALL)?;
    let layers = parse_names(p, "layers", &RowState::ALL)?;
    let f = &mut app.ui.layers_filter;
    if let Some(objects) = objects {
        f.objects = objects;
    }
    if let Some(layers) = layers {
        f.layers = layers;
    }
    Ok(json!({"objects": names(&ObjectKind::ALL, &f.objects), "layers": names(&RowState::ALL, &f.layers)}))
}

#[cfg(test)]
mod tests;
