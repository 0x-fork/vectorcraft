//! Graphs: the nine graph tools and Object → Graph (Type…, Data…).
//!
//! A graph is a group carrying a [`GraphSpec`]; its children (axes, labels, one group per series
//! with that series' legend swatch, like Illustrator's group-selectable series) are generated from
//! the spec. Editing the data or the type regenerates them in place, keeping any move/scale the
//! user applied to the graph since it was generated.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use vectorcraft_color::{Color, Paint};
use vectorcraft_doc::{
    Appearance, CharStyle, ColumnDesign, ColumnType, Document, Fractions, GraphDesign, GraphKind, GraphSpec, Justify, Node, NodeId, NodeKind,
    TextObject, TickLength, ValueAxisSide,
};
use vectorcraft_geom::{Affine, BezPath, FillRule, PathData, Point, Rect, shapes};

use super::typecmd::refresh_bounds;
use super::*;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "graph.create",
            "Create Graph",
            [],
            None,
            "{type: column|stackedColumn|bar|stackedBar|line|area|scatter|pie|radar, x, y, width, height, series?: [..], categories?: [..], rows?: [[number|null]], csv?} → {id}; an empty CSV cell or a null is a blank value, a number in straight quotes is a label",
            has_doc,
            create
        ),
        cmd!(
            "graph.setData",
            "Data…",
            ["Object", "Graph"],
            None,
            "{id?, series?, categories?, rows?, csv?: first row = series labels (first cell empty); then one row per category: label; values…; an empty cell or a null is a blank value; a quoted number a label; a `|` in a label a line break, transpose?: true swaps rows and columns (Transpose row/column), switchXY?: true swaps each scatter series' y and x columns (Switch x/y)} replace the graph's data; no data → the current {csv, series, categories, rows}",
            has_selection,
            set_data
        ),
        cmd!(
            "graph.setType",
            "Type…",
            ["Object", "Graph"],
            None,
            "{id?, type?, seriesIndexes?: [index], columnWidth?: % (1–1000; past 100 columns overlap), clusterWidth?: % (1–1000), legend?: bool, markPoints?: bool, connectPoints?: bool, edgeToEdge?: bool (line graphs: true runs the lines across the whole plot, false puts the points at the centres of their categories), ticks?: n, axisMin?, axisMax?, valueAxis?: left|right|both (the bar graphs' value axis stays along the bottom), separateScales?: bool, rightTicks?: n, rightAxisMin?, rightAxisMax?, tickLength?: none|short|full, tickMarks?: n per division, rightTickLength?, rightTickMarks?, categoryTickLength?: none|short|full, categoryTickMarks?: n, ticksBetweenLabels?: bool, prefix?, suffix?, rightPrefix?, rightSuffix?: text around the value axis numbers, firstRowInFront?: bool, firstColumnInFront?: bool (which overlapping columns are drawn on top: the first category over the next within each series, the first series over the others), filledLines?: bool (Draw Filled Lines: connecting lines as 3 pt bands filled with the series colour), dropShadow?: bool (Add Drop Shadow: a translucent black shadow down and right of the columns, bars, lines and pie wedges), legendAcrossTop?: bool (Add Legend Across Top: the legend in rows above the plot), pieLegend?: none|standard|wedges (Legends in Wedges: the series labels inside their wedges), piePosition?: ratio|even|stacked (several pies sized by their totals, at one size (the default), or stacked), pieSort?: all|first|none (wedges largest first in each pie, in the first pie's order, or in data order (the default)); the query lists the pie options for pie graphs} change the graph type and options; with `seriesIndexes`, or (no `id`) with only series selected with Group Selection, `type` goes to those series only and `valueAxis` (left|right; both is refused) puts them on that value axis, while the other options still apply to the whole graph (Combine different graph types: column, stacked column, line and area mix, and so do bar and stacked bar; a series given the graph's type follows the graph again) (axisMin and axisMax together override the calculated value axis: exactly that range in `ticks` divisions, 5 when 0; with the value axis on both sides, separateScales gives the series on the right axis a scale of their own, set the same way with rightTicks, rightAxisMin and rightAxisMax); no options → the current ones",
            has_selection,
            set_type
        ),
        cmd!(
            "graph.design",
            "Design…",
            ["Object", "Graph"],
            None,
            "{new?: name|true (New Design: the selected art becomes a graph design, named \"New Design\" when true; put a rectangle at the back to set the size it draws at), rename?: name, to?: new name (the graphs drawing it follow), paste?: name (Paste Design: a copy of the design's art into the document, selected, to edit and save again), delete?: name|[names] (graphs drawing them go back to their default marks), ids?|id?: object ids (default: the selection), selectUnused?: bool (Select Unused: just the list)}; no params or selectUnused → {designs: [names], unused: [the names no graph draws with]}",
            has_doc,
            design
        ),
        cmd!(
            "graph.column",
            "Column…",
            ["Object", "Graph"],
            None,
            "{id?, seriesIndexes?: [index], design?: name|null, columnType?: vertical|uniform|repeating|sliding, each?: value (more than 0), fractions?: chop|scale} draw the columns or bars and the legend swatch of the series selected with Group Selection (or listed, or every series) with a graph design, its backmost object as its frame and its guides left out: vertical stretches it along the value axis to each column's length at the column's width, uniform scales it both ways to that length, centered on the column, repeating stacks copies out from the zero line that each stand for `each` and cuts off the last one where the value ends (chop) or scales it down to fit (scale) (vertical when one copy would be longer than the canvas), and sliding scales the part above its horizontal guide line (in bar graphs, right of a vertical one) in proportion to the column's width and stretches the part below to fill the rest (vertical without a guide, or in a column too short for the part above); type in the design that holds a `%` followed by two digits shows each column's value in place of the `%` and the digits, the first digit giving the least number of places before the decimal point (zeros added in front; 0 = as many as the value needs) and the second digit giving the places after it (rounded half away from zero), at the type's own size where the design's copy puts it (a repeating design shows it once, where a whole copy ending at the value end puts it; type inside a group that clips, is hidden or has its own opacity, blend mode, isolation, knockout, appearance or opacity mask, or inside a blend, compound shape, envelope, repeat or intertwine, stays there and is filled in each copy, drawn with the design; type inside a symbol stays as typed), and is left out of the legend swatch (a design of such type alone gets the default swatch); the design keeps its own orientation for negative values, and columns drawn with a design cast no drop shadow; null = plain columns; options left out keep the series' shared ones, else vertical, 10 and chop. Column, stacked column, bar and stacked bar series. No design → {designs: [names], design: the selected series' design or null, columnType, each, fractions}",
            has_selection,
            set_column
        ),
        cmd!(
            "graph.marker",
            "Marker…",
            ["Object", "Graph"],
            None,
            "{id?, seriesIndexes?: [index], design?: name|null} draw the data points and legend swatch of the series selected with Group Selection (or listed, or every series) with a graph design, scaled so its backmost object fills the default marker's square; null = the default marker. Line, scatter and radar series. No design → {designs: [names], design: the selected series' design or null}",
            has_selection,
            set_marker
        ),
    ]
}

// ---------- data ----------

/// One CSV cell: its text, and whether it was in straight quotes (a quoted number is a label, not a value).
struct Cell {
    text: String,
    quoted: bool,
}

impl Cell {
    fn number(&self) -> Option<f64> {
        if self.quoted { None } else { self.text.parse::<f64>().ok().filter(|v| v.is_finite()) }
    }
}

/// Split `s` at each `sep` outside straight quotes (a `""` inside quotes toggles twice, so stays inside).
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = vec![];
    let mut quoted = false;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch == sep && !quoted {
            parts.push(s.get(start..i).unwrap_or_default());
            start = i + ch.len_utf8();
        }
    }
    parts.push(s.get(start..).unwrap_or_default());
    parts
}

/// One CSV cell: a cell wholly in quotes keeps its inner text (`""` = `"`), others are trimmed.
fn cell(c: &str) -> Cell {
    let c = c.trim();
    match c.strip_prefix('"').and_then(|c| c.strip_suffix('"')) {
        Some(inner) => Cell { text: inner.replace("\"\"", "\""), quoted: true },
        None => Cell { text: c.trim_matches('"').to_string(), quoted: false },
    }
}

fn parse_csv(csv: &str) -> (Vec<String>, Vec<String>, Vec<Vec<Option<f64>>>) {
    // Records end at line breaks outside quotes; a record is tab-separated if it has a tab outside quotes.
    let rows: Vec<Vec<Cell>> = split_unquoted(csv, '\n')
        .into_iter()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let tabs = split_unquoted(l, '\t');
            let parts = if tabs.len() > 1 { tabs } else { split_unquoted(l, ',') };
            parts.into_iter().map(cell).collect()
        })
        .collect();
    let Some(first) = rows.first() else { return (vec![], vec![], vec![]) };
    // A label is quoted or doesn't read as a number; an empty unquoted cell is a blank value.
    let label = |c: &Cell| c.quoted || (c.number().is_none() && !c.text.is_empty());
    // A header row has an empty first cell or a label after it, or is a label followed only by empty cells (`Year,,`).
    // All-empty series names mean no legend.
    let empty = |c: &Cell| c.text.is_empty() && !c.quoted;
    let header = first.first().is_some_and(empty)
        || first.iter().skip(1).any(label)
        || (first.len() > 1 && first.first().is_some_and(label) && first.iter().skip(1).all(empty));
    let mut series: Vec<String> = if header { first.iter().skip(1).map(|c| c.text.clone()).collect() } else { vec![] };
    if series.iter().all(String::is_empty) {
        series.clear();
    }
    let data = rows.get(usize::from(header)..).unwrap_or_default();
    // The first column holds the category labels if any row starts with one; then it does for every row, so a
    // row with an empty label doesn't shift its values one place left.
    let labelled = data.iter().any(|r| r.first().is_some_and(label));
    let mut categories = vec![];
    let mut values = vec![];
    for r in data {
        if labelled {
            categories.push(r.first().map(|c| c.text.clone()).unwrap_or_default());
        }
        values.push(r.iter().skip(labelled as usize).map(Cell::number).collect());
    }
    (series, categories, values)
}

fn to_csv(g: &GraphSpec) -> String {
    g.to_csv()
}

/// Apply CSV graph data to the graph root `id` and rebuild its art (Variables data merge).
pub(crate) fn apply_csv(d: &mut Document, id: NodeId, csv: &str) -> Result<()> {
    let Some(node) = d.node(id) else {
        return Err(EngineError::NoNode(id));
    };
    let Some(mut spec) = node.extras().graph.as_deref().cloned() else {
        return Err(bad("graph.applyCsv", format!("node {} is not a graph", id.0)));
    };
    let (series, categories, rows) = parse_csv(csv);
    spec.series = series;
    spec.categories = categories;
    spec.set_cells(rows);
    regenerate(d, id, spec)
}

fn apply_data(g: &mut GraphSpec, p: &Value) -> bool {
    let mut changed = false;
    if let Some(csv) = str_param(p, "csv") {
        let (s, c, r) = parse_csv(csv);
        g.series = s;
        g.categories = c;
        g.set_cells(r);
        changed = true;
    }
    let strings =
        |v: &Value| v.as_array().map(|a| a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>());
    if let Some(s) = p.get("series").and_then(strings) {
        g.series = s;
        changed = true;
    }
    if let Some(c) = p.get("categories").and_then(strings) {
        g.categories = c;
        changed = true;
    }
    if let Some(rows) = p.get("rows").and_then(Value::as_array) {
        // `null` (or anything that isn't a finite number) is a blank cell.
        g.set_cells(
            rows.iter().map(|r| r.as_array().map(|a| a.iter().map(|v| v.as_f64().filter(|v| v.is_finite())).collect()).unwrap_or_default()).collect(),
        );
        changed = true;
    }
    drop_empty_rows(g);
    changed
}

/// Drop rows with neither a value nor a label; a label-only row stays, its values blank.
fn drop_empty_rows(g: &mut GraphSpec) {
    let mut cells = g.cells();
    let keep: Vec<bool> = (0..cells.len())
        .map(|c| cells.get(c).is_some_and(|r| r.iter().any(Option::is_some)) || g.categories.get(c).is_some_and(|l| !l.is_empty()))
        .collect();
    if keep.contains(&false) || cells.len() < g.rows.len() {
        let mut k = keep.iter();
        cells.retain(|_| k.next().copied().unwrap_or(false));
        let mut k = keep.iter();
        g.categories.retain(|_| k.next().copied().unwrap_or(true));
        g.set_cells(cells);
    }
}

fn fmt_value(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        // Beyond the i64 range the cast would saturate; every f64 that large is a whole number, so
        // `{:.0}` prints its exact digits.
        if v.abs() < 9e18 { format!("{}", v.round() as i64) } else { format!("{v:.0}") }
    } else {
        format!("{:.2}", v).trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// ---------- generation ----------

/// Series colours: black, then greys (graphs start in greyscale).
fn default_series_color(i: usize) -> Color {
    const LEVELS: [f32; 8] = [0.0, 0.55, 0.8, 0.3, 0.68, 0.15, 0.9, 0.42];
    let l = LEVELS[i % LEVELS.len()];
    Color::rgb(l, l, l)
}

fn default_series_paint(i: usize) -> Paint {
    Paint::solid(default_series_color(i))
}

/// Fill and stroke for one generated mark of series `i`. A stored fill is used only where the
/// generator paints with the series colour (`series_fill`). A stored stroke replaces that part's
/// own stroke, so a column's stroke stays a stroke and a line's markers do not repaint its line.
fn series_marks(g: &GraphSpec, i: usize, series_fill: bool, stroke: Paint, width: f64) -> (Paint, Paint, f64) {
    let slot = g.series_paints.get(i);
    let fill = if series_fill { slot.and_then(|p| p.fill.clone()).unwrap_or_else(|| default_series_paint(i)) } else { Paint::None };
    let stroke = slot.and_then(|p| p.stroke.clone()).unwrap_or(stroke);
    let width = if slot.is_some_and(|p| p.stroke.is_some()) { slot.and_then(|p| p.stroke_width).unwrap_or(width) } else { width };
    (fill, stroke, width)
}

const LABEL_SIZE: f64 = 9.0;
/// Most tick marks per division (Graph Type › Tick Marks), and most category tick marks a graph draws.
const MAX_TICK_MARKS: usize = 20;
const MAX_CATEGORY_TICKS: usize = 10_000;
/// Longest axis label prefix or suffix (Graph Type › Add Labels).
const MAX_AFFIX: usize = 64;

/// An axis label prefix or suffix: one line (no control characters or line separators), at most [`MAX_AFFIX`] long.
fn one_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}')).take(MAX_AFFIX).collect()
}

/// A count parameter: a whole number, also as the float a dialog sends back (3.0), at most `max`.
fn count_param(p: &Value, key: &str, max: u64) -> Option<usize> {
    let v = p.get(key)?;
    let n = v.as_u64().or_else(|| v.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(|x| x.round().min(max as f64) as u64))?;
    usize::try_from(n.min(max)).ok()
}

/// A "nice" axis: (min, max, step).
fn nice_axis(lo: f64, hi: f64, ticks: usize) -> (f64, f64, f64) {
    let (lo, hi) = if (hi - lo).abs() < 1e-12 { (lo.min(0.0), lo.max(0.0) + 1.0) } else { (lo, hi) };
    let n = if ticks == 0 { 5 } else { ticks.clamp(1, 100) } as f64;
    let raw = (hi - lo) / n;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw - 1e-12).unwrap_or(10.0 * mag);
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

/// A value axis: with both `min` and `max` set (Graph Type › Tick Values › Override Calculated Values), exactly that
/// range split into `ticks` divisions (5 when automatic); otherwise a nice axis around the data, using whichever bound
/// was given.
fn value_axis(ticks: usize, axis_min: Option<f64>, axis_max: Option<f64>, lo: f64, hi: f64) -> (f64, f64, f64) {
    match (axis_min, axis_max) {
        (Some(min), Some(max)) if max > min && (max - min).is_finite() => {
            let n = if ticks == 0 { 5 } else { ticks.clamp(1, 100) } as f64;
            (min, max, (max - min) / n)
        }
        _ => {
            // An override too wide for f64 (or NaN) gives a non-finite axis, which would reach `clamp` with NaN bounds
            // and panic: fall back to the data, then to 0..1.
            let finite = |(a, b, s): (f64, f64, f64)| a.is_finite() && b.is_finite() && s.is_finite() && b > a && s > 0.0;
            [nice_axis(axis_min.unwrap_or(lo), axis_max.unwrap_or(hi), ticks), nice_axis(lo, hi, ticks)]
                .into_iter()
                .find(|&a| finite(a))
                .unwrap_or((0.0, 1.0, 0.2))
        }
    }
}

/// The tick values from `lo` to `hi` every `step`, both ends included: counted, not accumulated, so the last tick
/// lands on `hi` and a step too small for the range (or not finite) can't loop forever. At most 1000 ticks.
fn tick_values(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let n = ((hi - lo) / step).round();
    if !(lo.is_finite() && step.is_finite() && step > 0.0 && n.is_finite() && n >= 0.0) {
        return vec![];
    }
    (0..=(n as usize).min(1000)).map(|i| lo + i as f64 * step).collect()
}

struct Gen<'a> {
    d: &'a mut Document,
    out: Vec<Arc<Node>>,
    /// The labels' kept character styles.
    styles: &'a vectorcraft_doc::LabelStyles,
    /// What the column designs may still add.
    columns: Allowance,
}

/// What a graph label shows, for its kept character style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    Value,
    Category,
    Legend,
}

/// The character style labels of `role` are set in: the kept one, else 9 pt black.
fn label_style(styles: &vectorcraft_doc::LabelStyles, role: Role) -> CharStyle {
    let kept = match role {
        Role::Value => &styles.values,
        Role::Category => &styles.categories,
        Role::Legend => &styles.legend,
    };
    let mut st = kept.clone().unwrap_or_else(|| CharStyle { size: LABEL_SIZE, fill: Paint::solid(Color::BLACK), ..CharStyle::default() });
    // A file's style within the limits the Character controls keep to (it is copied onto every label).
    let within = |v: f64, lo: f64, hi: f64, or: f64| if v.is_finite() { v.clamp(lo, hi) } else { or };
    st.size = within(st.size, 0.1, 1296.0, LABEL_SIZE);
    st.leading = st.leading.map(|l| within(l, 0.1, 5000.0, LABEL_SIZE * 1.2));
    st.tracking = within(st.tracking, -1000.0, 10000.0, 0.0);
    st.h_scale = within(st.h_scale, 1.0, 10000.0, 100.0);
    st.v_scale = within(st.v_scale, 1.0, 10000.0, 100.0);
    st.stroke_width = within(st.stroke_width, 0.0, 1000.0, 0.0);
    st.font_family = st.font_family.chars().take(256).collect();
    st.font_style = st.font_style.chars().take(256).collect();
    st
}

/// How big labels of `role` are set: a kept style's size, else 9 pt. The labels' spacing follows it.
fn label_size(styles: &vectorcraft_doc::LabelStyles, role: Role) -> f64 {
    label_style(styles, role).size
}

impl Gen<'_> {
    fn path(&mut self, pd: PathData, fill: Paint, stroke: Paint, w: f64) -> Arc<Node> {
        Arc::new(Node::path(self.d.alloc_id(), pd, Appearance::basic(fill, stroke, w)))
    }
    fn line(&mut self, a: Point, b: Point) -> Arc<Node> {
        let mut bp = BezPath::new();
        bp.move_to(a);
        bp.line_to(b);
        self.path(PathData::from_bezpath(&bp), Paint::None, Paint::solid(Color::BLACK), 0.5)
    }
    fn text(&mut self, role: Role, at: Point, s: &str, justify: Justify) -> Arc<Node> {
        let style = label_style(self.styles, role);
        let mut t = TextObject::point(at, s, style);
        t.para.justify = justify;
        t.system_layout = self.d.setup.system_layout;
        refresh_bounds(&mut t);
        Arc::new(Node::new(self.d.alloc_id(), NodeKind::Text(Box::new(t))))
    }
    /// A category or series label: a `|` in it starts a new line (Add graph labels). `middle`: the lines are centred
    /// on `at` (labels beside an axis or in a wedge) instead of running down from it.
    fn label(&mut self, role: Role, at: Point, s: &str, justify: Justify, middle: bool) -> Arc<Node> {
        let lines = s.split('|').count();
        let at = if middle { Point::new(at.x, at.y - (lines - 1) as f64 * label_size(self.styles, role) * 1.2 / 2.0) } else { at };
        self.text(role, at, &s.replace('|', "\n"), justify)
    }
    fn group(&mut self, name: &str, children: Vec<Arc<Node>>) -> Arc<Node> {
        let mut g = Node::group(self.d.alloc_id(), children);
        g.name = Some(name.into());
        Arc::new(g)
    }
    /// A series' line joining its data points (Connect Data Points): a 1 pt stroke in the series colour, or with Draw
    /// Filled Lines a band FILLED_LINE_WIDTH wide filled with it and outlined in black.
    fn data_line(&mut self, g: &GraphSpec, s: usize, pd: PathData, filled: bool) -> Arc<Node> {
        let band = if filled {
            vectorcraft_pathops::outline_stroke(&pd, FILLED_LINE_WIDTH, vectorcraft_pathops::Cap::Butt, vectorcraft_pathops::Join::Miter, 4.0)
        } else {
            PathData::default()
        };
        // A run the outline can't be made for (its points all in one place) stays a thin line.
        if !band.is_empty() {
            // The band takes the series' colour, set as a fill or, on a thin line, as its stroke; a thin black outline.
            let slot = g.series_paints.get(s);
            let fill = slot.and_then(|p| p.fill.clone().or_else(|| p.stroke.clone())).unwrap_or_else(|| default_series_paint(s));
            return self.path(band, fill, Paint::solid(Color::BLACK), 0.25);
        }
        let (fill, stroke, w) = series_marks(g, s, false, default_series_paint(s), 1.0);
        self.path(pd, fill, stroke, w)
    }
    /// A data point's marker filling `cell`: the series' marker design, else the default square in the series' paint.
    fn marker(&mut self, design: Option<&Arc<Node>>, cell: Rect, (fill, stroke, w): (Paint, Paint, f64)) -> Arc<Node> {
        match design.and_then(|a| fit_design(self.d, a, cell)) {
            Some(n) => self.design_copy(n),
            None => self.path(shapes::rectangle(cell), fill, stroke, w),
        }
    }
    /// A legend swatch filling `cell`: the series' column design with its totals left out, else, and when the design
    /// draws nothing else, its [`Self::marker`].
    fn swatch(&mut self, column: Option<&ColumnArt>, marker: Option<&Arc<Node>>, cell: Rect, paint: (Paint, Paint, f64)) -> Arc<Node> {
        let fitted = match column.and_then(|c| Some((c, c.legend.as_ref()?))) {
            Some((c, legend)) if self.columns.take(legend.count()) => c.fit(self.d, legend, cell),
            _ => None,
        };
        match fitted {
            Some(n) => self.design_copy(n),
            None => self.marker(marker, cell, paint),
        }
    }
    /// A copy of design art drawn in a series: a single path gets a group of its own, as painting a series paints the
    /// paths directly inside its group and the design keeps its own paint.
    fn design_copy(&mut self, n: Arc<Node>) -> Arc<Node> {
        if matches!(n.kind, NodeKind::Path { .. }) { Arc::new(Node::group(self.d.alloc_id(), vec![n])) } else { n }
    }
    /// A series group. Empty series are omitted by the caller; the index is stored on the group
    /// so later paint edits find the series without counting siblings.
    fn series(&mut self, name: &str, index: usize, children: Vec<Arc<Node>>) -> Arc<Node> {
        let mut g = Node::group(self.d.alloc_id(), children);
        g.name = Some(name.into());
        g.extras_mut().series_index = u32::try_from(index).ok();
        Arc::new(g)
    }
}

/// Add Drop Shadow: how far the shadow sits down and right of its mark, and how dark it is.
const SHADOW_OFFSET: f64 = 2.0;
const SHADOW_OPACITY: f32 = 0.35;
/// Draw Filled Lines: how wide a filled line is.
const FILLED_LINE_WIDTH: f64 = 3.0;
const FILLED_LINE_BUDGET: usize = 20_000;
/// Most marks a graph draws shadows for (each is a copy).
const SHADOW_BUDGET: usize = 200_000;

/// Mark `n`'s shadow: a black copy moved down and right, its fill and stroke (whichever it has) black.
fn shadow_of(d: &mut Document, n: &Node) -> Arc<Node> {
    // Generated marks carry only paint and a stroke width, so that's all a shadow keeps.
    let mut c = n.clone();
    c.transform(Affine::translate((SHADOW_OFFSET, SHADOW_OFFSET)), false);
    let fill = if c.appearance.fill_paint().is_none() { Paint::None } else { Paint::solid(Color::BLACK) };
    let stroke = if c.appearance.stroke_paint().is_none() { Paint::None } else { Paint::solid(Color::BLACK) };
    c.appearance = Appearance::basic(fill, stroke, c.appearance.stroke_width()).into();
    Arc::new(d.reid(&c))
}

/// How wide label `s` is set (its widest line), without adding it to a document.
fn label_width(styles: &vectorcraft_doc::LabelStyles, s: &str) -> f64 {
    let style = label_style(styles, Role::Legend);
    let mut t = TextObject::point(Point::ZERO, &s.replace('|', "\n"), style);
    refresh_bounds(&mut t);
    Node::new(NodeId(0), NodeKind::Text(Box::new(t))).geometric_bounds().map_or(0.0, |b| b.width())
}

/// A label with nothing to show (empty, or only `|` line breaks).
fn blank_label(s: &str) -> bool {
    s.chars().all(|c| c == '|')
}

/// Text node `t` with its characters painted `colour`.
fn recolour_text(t: &Arc<Node>, colour: Color) -> Arc<Node> {
    let mut n = (**t).clone();
    if let NodeKind::Text(text) = &mut n.kind {
        for run in &mut text.runs {
            run.style.fill = Paint::solid(colour);
        }
    }
    Arc::new(n)
}

/// The stretches of two or more points between blank cells, which a line connects.
fn runs(pts: &[Option<Point>]) -> Vec<Vec<Point>> {
    pts.split(Option::is_none).map(|r| r.iter().flatten().copied().collect::<Vec<_>>()).filter(|r| r.len() > 1).collect()
}

fn polyline(pts: &[Point], closed: bool) -> PathData {
    let mut bp = BezPath::new();
    for (i, p) in pts.iter().enumerate() {
        if i == 0 {
            bp.move_to(*p);
        } else {
            bp.line_to(*p);
        }
    }
    if closed {
        bp.close_path();
    }
    PathData::from_bezpath(&bp)
}

/// Most objects a graph's marker designs may add (each data point and legend swatch draws a copy of its design).
const MARKER_BUDGET: usize = 200_000;

/// Each series' marker design art, when the document has the design.
fn marker_designs(d: &Document, g: &GraphSpec) -> Vec<Option<Arc<Node>>> {
    let nser = g.rows.iter().map(Vec::len).max().unwrap_or(0).clamp(1, vectorcraft_doc::MAX_GRAPH_SERIES);
    (0..nser)
        .map(|s| {
            let name = g.series_markers.get(s)?.as_deref()?;
            d.graph_designs.iter().find(|x| x.name == name).map(|x| x.art.clone())
        })
        .collect()
}

/// How many objects the marker designs `designs` add to graph `g`: a copy per data point and legend swatch.
fn marker_cost(g: &GraphSpec, designs: &[Option<Arc<Node>>]) -> usize {
    let points = g.rows.len().min(vectorcraft_doc::MAX_GRAPH_CATEGORIES) + 1;
    designs.iter().flatten().map(|a| a.count().saturating_mul(points)).fold(0, usize::saturating_add)
}

/// The frame graph design `art` is drawn around: its backmost object's bounds (the whole art's when the design is a
/// single object). `None` for art with no area.
fn design_frame(art: &Node) -> Option<Rect> {
    let usable = |b: &Rect| b.width() > 1e-9 && b.height() > 1e-9 && b.width().is_finite() && b.height().is_finite();
    let group = matches!(art.kind, NodeKind::Group { .. });
    let backmost = art.children().filter(|_| group).and_then(|c| c.first()).and_then(|c| c.geometric_bounds()).filter(usable);
    backmost.or_else(|| art.geometric_bounds().filter(usable))
}

/// Graph design `art` scaled into `cell` (Object › Graph › Marker): its backmost object, the rectangle a design is
/// drawn around, fills the cell (the whole art does when the design is a single object), strokes scaling with it.
/// Fresh ids. `None` for art with no area.
fn fit_design(d: &mut Document, art: &Node, cell: Rect) -> Option<Arc<Node>> {
    let a = fit_affine(design_frame(art)?, cell);
    // Strokes, effects and pattern tiles scale with the art.
    let art = super::place::transformed(art.clone(), a);
    Some(Arc::new(d.reid(&art)))
}

/// The map that puts a design's frame `frame` on `cell` ([`fit_design`]).
fn fit_affine(frame: Rect, cell: Rect) -> Affine {
    Affine::translate(cell.origin().to_vec2())
        * Affine::scale_non_uniform(cell.width() / frame.width(), cell.height() / frame.height())
        * Affine::translate(-frame.origin().to_vec2())
}

/// Most objects a graph's column designs may add (each column, repeated copy and legend swatch draws a copy).
const COLUMN_BUDGET: usize = 200_000;

/// A series' column design (Object › Graph › Column): its options; its art without guide lines and without the totals
/// taken out of it ([`without_totals`]); the bounds of those guides; those totals; its frame (the art's without guides,
/// totals included, [`design_frame`]); whether its art still holds totals (inside groups that do more than group, or
/// inside other objects that hold objects), which are filled where they are; and its art without any total for the
/// legend swatch, `None` when that draws nothing.
struct ColumnArt {
    options: ColumnDesign,
    art: Node,
    guides: Vec<Rect>,
    totals: Vec<Node>,
    frame: Option<Rect>,
    in_place: bool,
    legend: Option<Node>,
}

impl ColumnArt {
    /// A copy of `art` with the design's frame fitted to `cell`, as [`fit_design`] fits it. Fresh ids.
    fn fit(&self, d: &mut Document, art: &Node, cell: Rect) -> Option<Arc<Node>> {
        let art = super::place::transformed(art.clone(), fit_affine(self.frame?, cell));
        Some(Arc::new(d.reid(&art)))
    }
}

/// The column design totals in `s` (Add totals to a column design): each `%` followed by two digits, as its byte offset,
/// the places before the decimal point (the first digit) and the places after it (the second), left to right. A `%` not
/// followed by two digits is plain text.
fn total_spans(s: &str) -> Vec<(usize, usize, usize)> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while let Some(w) = b.get(i..i + 3) {
        match *w {
            [b'%', x, y] if x.is_ascii_digit() && y.is_ascii_digit() => {
                out.push((i, usize::from(x - b'0'), usize::from(y - b'0')));
                i += 3;
            }
            _ => i += 1,
        }
    }
    out
}

/// Value `v` as a column design total shows it: rounded half away from zero, from its shortest decimal form, to `after`
/// places after the decimal point, zeros added as needed; and with at least `before` places before the point, zeros
/// added in front (0: as many as the value needs). The places before the point are never cut, and a value that rounds
/// to 0 has no minus sign.
fn total_text(v: f64, before: usize, after: usize) -> String {
    if !v.is_finite() {
        return fmt_value(v);
    }
    let s = format!("{}", v.abs());
    let (int, frac) = s.split_once('.').unwrap_or((s.as_str(), ""));
    // The digits kept, the point `after` places from the end.
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().chain(std::iter::repeat(b'0')).take(after)).collect();
    if frac.as_bytes().get(after).is_some_and(|d| *d >= b'5') {
        let mut carry = true;
        for d in digits.iter_mut().rev() {
            if *d == b'9' {
                *d = b'0';
            } else {
                *d += 1;
                carry = false;
                break;
            }
        }
        if carry {
            digits.insert(0, b'1');
        }
    }
    let (whole, places) = digits.split_at(digits.len().saturating_sub(after));
    let mut out = String::new();
    if v < 0.0 && digits.iter().any(|d| *d != b'0') {
        out.push('-');
    }
    out.extend(std::iter::repeat_n('0', before.saturating_sub(whole.len())));
    out.extend(whole.iter().map(|d| char::from(*d)));
    if after > 0 {
        out.push('.');
        out.extend(places.iter().map(|d| char::from(*d)));
    }
    out
}

/// Type `t` with each total in it ([`total_spans`]) replaced by value `v` ([`total_text`]), styled as its `%` was; true
/// when it held one.
fn fill_totals(t: &mut TextObject, v: f64) -> bool {
    let spans = total_spans(&t.plain_text());
    // Last first, so the offsets of earlier ones stay valid.
    for &(at, before, after) in spans.iter().rev() {
        let s = total_text(v, before, after);
        t.splice_paras(at, at + 3, &s);
        vectorcraft_text::edit::replace_range(&mut t.runs, at, at + 3, &s);
    }
    if !spans.is_empty() {
        refresh_bounds(t);
    }
    !spans.is_empty()
}

/// Is `n` type that holds a total ([`total_spans`])?
fn holds_total(n: &Node) -> bool {
    matches!(&n.kind, NodeKind::Text(t) if !total_spans(&t.plain_text()).is_empty())
}

/// Does group `n` only group its children: no clipping, hiding, opacity, blend mode, isolation, knockout, appearance of
/// its own or opacity mask, so that type taken out of it draws as it did inside it?
fn only_groups(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Group { clip: false, .. })
        && n.visible
        && n.opacity == 1.0
        && n.blend == vectorcraft_color::BlendMode::Normal
        && !n.isolate
        && n.knockout == vectorcraft_doc::Knockout::default()
        && n.appearance.items.is_empty()
        && n.appearance.effects.is_empty()
        && n.extras().mask.is_none()
}

/// Is `n` a clipping group whose clipping path is type that holds a total?
fn clipped_by_total(n: &Node) -> bool {
    matches!(&n.kind, NodeKind::Group { clip: true, children } if children.first().is_some_and(|c| holds_total(c)))
}

/// `art` without the type in it that holds a total, inside groups at any depth that only group ([`only_groups`]), and
/// that type, pushed to `totals` in paint order (Add totals to a column design); with `all`, without every total inside
/// any group or other object that holds objects (a blend, compound shape, envelope, repeat or intertwine), and with
/// each group that such type clips left empty. A design that is such type alone leaves an empty group.
fn without_totals(art: &Node, all: bool, totals: &mut Vec<Node>) -> Node {
    if holds_total(art) || (all && clipped_by_total(art)) {
        totals.push(art.clone());
        return Node::group(art.id, vec![]);
    }
    let mut n = art.clone();
    let take = all || only_groups(&n);
    if take && let Some(children) = n.children_mut() {
        for c in std::mem::take(children) {
            if holds_total(&c) {
                totals.push((*c).clone());
            } else if c.children().is_some() {
                children.push(Arc::new(without_totals(&c, all, totals)));
            } else {
                children.push(c);
            }
        }
    }
    n
}

/// `n` with the totals in it at any depth, in groups and the other objects that hold objects, showing value `v` where
/// they are ([`fill_totals`]).
fn fill_in_place(n: &mut Node, v: f64) {
    if let NodeKind::Text(t) = &mut n.kind {
        fill_totals(t, v);
    } else if let Some(children) = n.children_mut() {
        for c in children.iter_mut() {
            fill_in_place(Arc::make_mut(c), v);
        }
    }
}

/// `art` without the paths made into guides, at any depth.
fn without_guides(art: &Node) -> Node {
    let mut n = art.clone();
    if let Some(ch) = n.children_mut() {
        let old = std::mem::take(ch);
        *ch = old.iter().filter(|c| !matches!(c.kind, NodeKind::Path { guide: true, .. })).map(|c| Arc::new(without_guides(c))).collect();
    }
    n
}

/// Each series' column design, when the document has the design.
fn column_designs(d: &Document, g: &GraphSpec) -> Vec<Option<ColumnArt>> {
    let nser = g.rows.iter().map(Vec::len).max().unwrap_or(0).clamp(1, vectorcraft_doc::MAX_GRAPH_SERIES);
    (0..nser)
        .map(|s| {
            let options = g.series_columns.get(s)?.as_ref()?;
            let art = &d.graph_designs.iter().find(|x| x.name == options.design)?.art;
            let mut guides = vec![];
            art.walk(&mut |n| {
                if matches!(n.kind, NodeKind::Path { guide: true, .. })
                    && let Some(b) = n.geometric_bounds()
                {
                    guides.push(b);
                }
            });
            let art = without_guides(art);
            let frame = design_frame(&art);
            let legend = without_totals(&art, true, &mut vec![]);
            let mut draws = false;
            legend.walk(&mut |n| draws |= n.children().is_none());
            let mut totals = vec![];
            let art = without_totals(&art, false, &mut totals);
            let mut in_place = false;
            art.walk(&mut |n| in_place |= holds_total(n));
            Some(ColumnArt { options: options.clone(), art, guides, totals, frame, in_place, legend: draws.then_some(legend) })
        })
        .collect()
}

/// The value one copy of a repeating design stands for, when the design repeats and the value is usable.
fn repeat_each(c: &ColumnDesign) -> Option<f64> {
    (c.column_type == ColumnType::Repeating && c.each.is_finite() && c.each > 0.0).then_some(c.each)
}

/// The objects a graph's column designs may still add (out of [`COLUMN_BUDGET`]); `short` once a copy needed more than
/// was left, and the graph is then drawn with plain columns.
struct Allowance {
    left: usize,
    short: bool,
}

impl Allowance {
    fn new() -> Self {
        Self { left: COLUMN_BUDGET, short: false }
    }
    /// Spend `n` objects, when that many are left.
    fn take(&mut self, n: usize) -> bool {
        match self.left.checked_sub(n) {
            Some(rest) => {
                self.left = rest;
                true
            }
            None => {
                self.short = true;
                false
            }
        }
    }
}

/// Column or bar `rect` of value `value` drawn with column design `c` (Object › Graph › Column), as a group of fresh art,
/// its copies spent from `left`. `far_low`: the value end of the column is at the rect's lower coordinate (the top of a
/// column above zero, the left end of a bar below zero), so repeating copies stack from the other end. `per_unit`: plot
/// points per unit of value. The design's totals show `value` and come last: each keeps its size and is moved to where
/// the design's copy puts the point it is set from; in a repeating design they are drawn once, where a whole copy ending
/// at the value end puts them, and a sliding design moves each with the part of the design on its side of the guide.
/// Totals left in the art ([`ColumnArt`]) show `value` in each copy, drawn with it. `None` for art with no area, for a
/// column of no length and when `left` runs out, which are drawn plain.
fn column_art(
    d: &mut Document,
    c: &ColumnArt,
    (rect, value): (Rect, f64),
    (horizontal, far_low): (bool, bool),
    per_unit: f64,
    left: &mut Allowance,
) -> Option<Arc<Node>> {
    let frame = c.frame?;
    let length = |r: Rect| if horizontal { r.width() } else { r.height() };
    let thickness = |r: Rect| if horizontal { r.height() } else { r.width() };
    if length(rect) <= 1e-9 || !length(rect).is_finite() {
        return None;
    }
    let cost = c.art.count_with_masks();
    // A copy of the design moved by `a`, the totals left in it showing `value`; a copy fitted to `cell`; and a copy moved
    // by `a` and clipped to what it paints, cut at `at` (an x when `along_x`, else a y) with the side below `at` kept when
    // `keep_low` (two objects more).
    let copy = |d: &mut Document, a: Affine| {
        let mut art = d.reid(&super::place::transformed(c.art.clone(), a));
        if c.in_place {
            fill_in_place(&mut art, value);
        }
        art
    };
    let fit = |d: &mut Document, cell: Rect, left: &mut Allowance| left.take(cost).then(|| Arc::new(copy(d, fit_affine(frame, cell))));
    let clipped = |d: &mut Document, a: Affine, (along_x, keep_low, at): (bool, bool, f64), left: &mut Allowance| {
        let art = left.take(cost + 2).then(|| copy(d, a))?;
        // The box of the copy and of each object in it that isn't a group or layer, as the brush art past a brushed
        // stroke counts only in its own object's box.
        let mut b = vectorcraft_render::painted_bounds(d, &art);
        art.walk(&mut |n| {
            if !matches!(n.kind, NodeKind::Group { .. } | NodeKind::Layer { .. }) {
                b = vectorcraft_geom::union_opt(b, vectorcraft_render::painted_bounds(d, n));
            }
        });
        // A copy that draws nothing (a design of totals alone) needs no clip.
        let Some(b) = b.or_else(|| art.geometric_bounds()) else { return Some(Arc::new(art)) };
        let keep = match (along_x, keep_low) {
            (true, true) => Rect::new(b.x0, b.y0, at.max(b.x0), b.y1),
            (true, false) => Rect::new(at.min(b.x1), b.y0, b.x1, b.y1),
            (false, true) => Rect::new(b.x0, b.y0, b.x1, at.max(b.y0)),
            (false, false) => Rect::new(b.x0, at.min(b.y1), b.x1, b.y1),
        };
        Some(Arc::new(super::rasterfx::clip_group(d, shapes::rectangle(keep), FillRule::NonZero, art)))
    };
    // The part of the column from `a` to `b` points out from its zero end.
    let piece = |a: f64, b: f64| match (horizontal, far_low) {
        (true, true) => Rect::new(rect.x1 - b, rect.y0, rect.x1 - a, rect.y1),
        (true, false) => Rect::new(rect.x0 + a, rect.y0, rect.x0 + b, rect.y1),
        (false, true) => Rect::new(rect.x0, rect.y1 - b, rect.x1, rect.y1 - a),
        (false, false) => Rect::new(rect.x0, rect.y0 + a, rect.x1, rect.y0 + b),
    };
    // The column's group: `parts`, then the totals, each showing `value` at its own size, moved as `a` moves the point it
    // is set from; with `sliding` (the kept part's map, the stretched part's map and the guide, as [`slide`] gives them),
    // as the map of the part on its side of the guide moves it. One object each.
    let group = |d: &mut Document, mut parts: Vec<Arc<Node>>, a: Affine, sliding: Option<(Affine, Affine, f64)>, left: &mut Allowance| {
        for t in &c.totals {
            if !left.take(1) {
                return None;
            }
            let mut n = t.clone();
            let mut by = Affine::IDENTITY;
            if let NodeKind::Text(text) = &mut n.kind {
                // A move only, so the type keeps its size; pattern paints move with it.
                let p = text.xf * Point::ZERO;
                let a = match sliding {
                    Some((kept, stretched, at)) => {
                        let q = kept * p;
                        if (horizontal && q.x >= at) || (!horizontal && q.y <= at) { kept } else { stretched }
                    }
                    None => a,
                };
                fill_totals(text, value);
                by = Affine::translate(a * p - p);
            }
            parts.push(Arc::new(d.reid(&super::place::transformed(n, by))));
        }
        Some(Arc::new(Node::group(d.alloc_id(), parts)))
    };
    let mut parts = vec![];
    let (mut label, mut sliding) = (fit_affine(frame, rect), None);
    match c.options.column_type {
        ColumnType::Uniform => {
            // Scaled both ways to the column's length, centered across it.
            let t = thickness(frame) * length(rect) / length(frame) / 2.0;
            let cell = if horizontal {
                Rect::new(rect.x0, rect.center().y - t, rect.x1, rect.center().y + t)
            } else {
                Rect::new(rect.center().x - t, rect.y0, rect.center().x + t, rect.y1)
            };
            parts.push(fit(d, cell, left)?);
            label = fit_affine(frame, cell);
        }
        ColumnType::Repeating => {
            // When the value per copy is unusable, too small to draw or makes one copy longer than the canvas, the design
            // is drawn vertically scaled.
            let Some(unit) = repeat_each(&c.options).map(|e| e * per_unit).filter(|u| u.is_finite() && *u > 1e-9 && *u <= crate::MAX_COORD) else {
                parts.push(fit(d, rect, left)?);
                return group(d, parts, label, None, left);
            };
            // The totals go where a whole copy ending at the value end puts them.
            label = fit_affine(frame, piece(length(rect) - unit, length(rect)));
            // The whole copies, and the points of the column left for the last one (none under a millionth of a point).
            let whole = ((length(rect) + 1e-6) / unit).floor();
            let rest = length(rect) - whole * unit;
            // Each copy is spent from `left`, which stops the loop.
            for i in 0..whole as usize {
                parts.push(fit(d, piece(i as f64 * unit, (i + 1) as f64 * unit), left)?);
            }
            if rest > 1e-6 {
                let start = whole * unit;
                match c.options.fractions {
                    Fractions::Scale => parts.push(fit(d, piece(start, start + rest), left)?),
                    Fractions::Chop => {
                        // The last copy cut off where the column ends, and nowhere else.
                        let cut = match (horizontal, far_low) {
                            (true, true) => (true, false, rect.x0),
                            (true, false) => (true, true, rect.x1),
                            (false, true) => (false, false, rect.y0),
                            (false, false) => (false, true, rect.y1),
                        };
                        parts.push(clipped(d, fit_affine(frame, piece(start, start + unit)), cut, left)?);
                    }
                }
            }
        }
        ColumnType::Sliding => match slide(frame, &c.guides, rect, horizontal) {
            Some((kept, stretched, at)) => {
                // Each copy cut at the guide, on its own side.
                parts.push(clipped(d, kept, (horizontal, !horizontal, at), left)?);
                parts.push(clipped(d, stretched, (horizontal, horizontal, at), left)?);
                sliding = Some((kept, stretched, at));
            }
            None => parts.push(fit(d, rect, left)?),
        },
        ColumnType::Vertical => parts.push(fit(d, rect, left)?),
    }
    group(d, parts, label, sliding, left)
}

/// Sliding design with frame `frame` in column `rect`, at the column's width: the map of the part above the first
/// horizontal guide in `guides` inside the frame (in a bar, right of the first vertical one), scaled in proportion and
/// put at the rect's top (in a bar, its right end), the value end for values above zero; the map of the part below (left
/// of) it, stretched or compressed to fill the rest; and where the guide lands (y, or x in a bar). Both maps put the guide there. `None` without such a guide, or when the column is too
/// short for the part kept in proportion.
fn slide(frame: Rect, guides: &[Rect], rect: Rect, horizontal: bool) -> Option<(Affine, Affine, f64)> {
    // A horizontal line (in a bar, a vertical one): longer than it is across, and at most a hundredth of the frame across.
    let inside = |g: &&Rect| {
        if horizontal {
            g.width() < g.height() && g.width() <= frame.width() / 100.0 && g.center().x > frame.x0 && g.center().x < frame.x1
        } else {
            g.height() < g.width() && g.height() <= frame.height() / 100.0 && g.center().y > frame.y0 && g.center().y < frame.y1
        }
    };
    let g = guides.iter().find(inside)?.center();
    if horizontal {
        let s = rect.height() / frame.height();
        let (kept, stretched) = (frame.x1 - g.x, g.x - frame.x0);
        let rest = rect.width() - kept * s;
        if !(rest.is_finite() && rest > 1e-9) {
            return None;
        }
        let k = rest / stretched;
        let kept_map = Affine::new([s, 0.0, 0.0, s, rect.x1 - frame.x1 * s, rect.y0 - frame.y0 * s]);
        let stretched_map = Affine::new([k, 0.0, 0.0, s, rect.x0 - frame.x0 * k, rect.y0 - frame.y0 * s]);
        Some((kept_map, stretched_map, rect.x1 - kept * s))
    } else {
        let s = rect.width() / frame.width();
        let (kept, stretched) = (g.y - frame.y0, frame.y1 - g.y);
        let rest = rect.height() - kept * s;
        if !(rest.is_finite() && rest > 1e-9) {
            return None;
        }
        let k = rest / stretched;
        let kept_map = Affine::new([s, 0.0, 0.0, s, rect.x0 - frame.x0 * s, rect.y0 - frame.y0 * s]);
        let stretched_map = Affine::new([s, 0.0, 0.0, k, rect.x0 - frame.x0 * s, rect.y1 - frame.y1 * k]);
        Some((kept_map, stretched_map, rect.y0 + kept * s))
    }
}

/// Build the graph's children: with each series' column design (Object › Graph › Column) when the document has it,
/// and with plain columns for all when the designs would draw more than COLUMN_BUDGET objects.
fn generate(d: &mut Document, g: &GraphSpec) -> Vec<Arc<Node>> {
    let column_looks = column_designs(d, g);
    match generate_with(d, g, &column_looks) {
        Some(out) => out,
        None => generate_with(d, g, &[]).unwrap_or_default(),
    }
}

/// [`generate`] with the column designs `column_looks`; `None` when they would draw more than COLUMN_BUDGET objects.
fn generate_with(d: &mut Document, g: &GraphSpec, column_looks: &[Option<ColumnArt>]) -> Option<Vec<Arc<Node>>> {
    let mark = |i: usize, series_fill: bool, stroke: Paint, width: f64| series_marks(g, i, series_fill, stroke, width);
    // Each series' marker design (Object › Graph › Marker), when the document has it; default squares for all
    // when the designs would draw more than MARKER_BUDGET objects.
    let mut designs = marker_designs(d, g);
    if marker_cost(g, &designs) > MARKER_BUDGET {
        designs.clear();
    }
    let design = |s: usize| designs.get(s).and_then(Option::as_ref);
    let column_look = |s: usize| column_looks.get(s).and_then(Option::as_ref);
    let mut b = Gen { d, out: vec![], styles: &g.label_styles, columns: Allowance::new() };
    // The labels' sizes (9 pt unless formatted), which their spacing follows.
    let (vs, cs, ls) =
        (label_size(&g.label_styles, Role::Value), label_size(&g.label_styles, Role::Category), label_size(&g.label_styles, Role::Legend));
    let r = g.rect;
    let nser = g.rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let ncat = g.rows.len().max(1);
    let series_label = |i: usize| g.series.get(i).cloned().unwrap_or_default();
    let mut series: Vec<Vec<Arc<Node>>> = vec![vec![]; nser];
    // A blank cell (no value) is `None`: columns, bars and stacked segments leave it out, lines and scatter points
    // break around it and the value axis ignores it. Area, pie and radar draw it as 0, which is what it adds to them.
    let cells = g.cells();
    let cell = |c: usize, s: usize| cells.get(c).and_then(|r| r.get(s)).copied().flatten();
    let val = |c: usize, s: usize| cell(c, s).unwrap_or(0.0);
    // Each series' own type (Combine different graph types); one type for the whole graph unless a series has its own.
    let kinds: Vec<GraphKind> = (0..nser).map(|s| g.series_kind(s)).collect();
    let kind = |s: usize| kinds.get(s).copied().unwrap_or(g.kind);
    // Series drawn as columns or bars, which column designs apply to.
    let columnar = |s: usize| matches!(kind(s), GraphKind::Column | GraphKind::StackedColumn | GraphKind::Bar | GraphKind::StackedBar);
    let horizontal = matches!(g.kind, GraphKind::Bar | GraphKind::StackedBar);
    // Where the art right of the plot ends (the right value axis' labels), so the legend starts after it.
    let mut right_edge = r.x1;

    // Labels drawn over the series: a pie's Legends in Wedges (in the Legend group) and stacked pies' names.
    let mut wedge_labels = vec![];
    let mut on_top = vec![];
    // Draw Filled Lines, up to FILLED_LINE_BUDGET data points (the outlines are rebuilt on every change).
    let filled = g.filled_lines && g.rows.len().saturating_mul(nser) <= FILLED_LINE_BUDGET;
    // Where columns and bars reach (past the plot when their widths are over 100%), so the legend keeps clear.
    let mut bars_reach: Option<Rect> = None;
    // Add Drop Shadow: the columns, bars, lines and wedges to cast one.
    let mut shadowed: Vec<(usize, Arc<Node>)> = vec![];
    match g.kind {
        GraphKind::Pie => {
            // One pie per row, a wedge per series, clockwise from 12 o'clock. Position: side by side at one size
            // (Even) or sized by their totals (Ratio, by area), or stacked on one centre, the largest at the back.
            let totals: Vec<f64> = (0..ncat).map(|c| (0..nser).map(|s| val(c, s).max(0.0)).sum()).collect();
            let biggest = totals.iter().copied().fold(0.0, f64::max);
            let stacked = g.pie_position == vectorcraft_doc::PiePosition::Stacked;
            let w = if stacked { r.width() } else { r.width() / ncat as f64 };
            let full = (w.min(r.height()) / 2.0 * 0.9).max(1.0);
            let radius = |total: f64| match g.pie_position {
                vectorcraft_doc::PiePosition::Even => full,
                _ if biggest > 0.0 && biggest.is_finite() => (full * (total / biggest).sqrt()).max(1.0),
                _ => full,
            };
            // Sort: data order, largest first in each pie, or the first pie's largest-first order everywhere.
            let order = |c: usize| {
                let mut o: Vec<usize> = (0..nser).collect();
                let by = match g.pie_sort {
                    vectorcraft_doc::PieSort::None => return o,
                    vectorcraft_doc::PieSort::All => c,
                    vectorcraft_doc::PieSort::First => 0,
                };
                o.sort_by(|a, b| val(by, *b).max(0.0).total_cmp(&val(by, *a).max(0.0)));
                o
            };
            let mut pies: Vec<usize> = (0..ncat).collect();
            if stacked {
                pies.sort_by(|a, b| totals.get(*b).copied().unwrap_or(0.0).total_cmp(&totals.get(*a).copied().unwrap_or(0.0)));
            }
            // Stacked pies share a centre: each one but the smallest is drawn as the ring outside the next smaller
            // one, so a pie never covers another (a pie as big as the next one has no ring and is left out).
            let inner: Vec<f64> = if stacked {
                (0..pies.len()).map(|k| pies.get(k + 1).map_or(0.0, |c| radius(totals.get(*c).copied().unwrap_or(0.0)))).collect()
            } else {
                vec![0.0; pies.len()]
            };
            for (k, c) in pies.into_iter().enumerate() {
                let total = totals.get(c).copied().unwrap_or(0.0);
                let rad = radius(total);
                let hole = inner.get(k).copied().unwrap_or(0.0);
                if hole >= rad - 1e-9 {
                    continue;
                }
                let centre = if stacked { r.center() } else { Point::new(r.x0 + w * (c as f64 + 0.5), r.y0 + r.height() / 2.0) };
                let mut a0 = -std::f64::consts::FRAC_PI_2;
                for s in order(c) {
                    let v = val(c, s).max(0.0);
                    if total <= 0.0 || v <= 0.0 {
                        continue;
                    }
                    let sweep = v / total * std::f64::consts::TAU;
                    let mut bp = BezPath::new();
                    let dir = |a: f64| vectorcraft_geom::Vec2::new(a.cos(), a.sin());
                    let arc = vectorcraft_geom::kurbo::Arc::new(centre, (rad, rad), a0, sweep, 0.0);
                    if hole > 0.0 {
                        // A ring segment: out along the outer arc, back along the inner one.
                        bp.move_to(centre + dir(a0) * rad);
                        arc.to_cubic_beziers(0.1, |p1, p2, p| bp.curve_to(p1, p2, p));
                        bp.line_to(centre + dir(a0 + sweep) * hole);
                        let back = vectorcraft_geom::kurbo::Arc::new(centre, (hole, hole), a0 + sweep, -sweep, 0.0);
                        back.to_cubic_beziers(0.1, |p1, p2, p| bp.curve_to(p1, p2, p));
                    } else {
                        bp.move_to(centre);
                        bp.line_to(centre + dir(a0) * rad);
                        arc.to_cubic_beziers(0.1, |p1, p2, p| bp.curve_to(p1, p2, p));
                    }
                    bp.close_path();
                    let (fill, stroke, sw) = mark(s, true, Paint::solid(Color::WHITE), 0.5);
                    // White text on a dark wedge.
                    let dark = fill.color().is_some_and(|c| {
                        let [r, g, b] = c.to_rgb();
                        0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5
                    });
                    let n = b.path(PathData::from_bezpath(&bp), fill, stroke, sw);
                    shadowed.push((s, n.clone()));
                    if let Some(items) = series.get_mut(s) {
                        items.push(n);
                    }
                    // Legends in Wedges: the series label inside its wedge, centred two thirds of the way out.
                    if g.legend && g.pie_legend_in_wedges {
                        let label = series_label(s);
                        if !blank_label(&label) {
                            // Two thirds of the way out (of the ring, for a stacked pie).
                            let mid = a0 + sweep / 2.0;
                            let at = centre + dir(mid) * (hole + (rad - hole) * 0.62);
                            let t = b.label(Role::Legend, Point::new(at.x, at.y + ls * 0.35), &label, Justify::Center, true);
                            // White on a dark wedge, unless the legend labels were given a colour of their own.
                            let own_colour = g.label_styles.legend.as_ref().is_some_and(|st| st.fill != Paint::solid(Color::BLACK));
                            wedge_labels.push(if dark && !own_colour { recolour_text(&t, Color::WHITE) } else { t });
                        }
                    }
                    a0 += sweep;
                }
                if let Some(cat) = g.categories.get(c).filter(|c| !c.is_empty()) {
                    if stacked {
                        // Just above its own outline, all its lines, and drawn over the pies.
                        let lines = cat.split('|').count() as f64;
                        let at = Point::new(centre.x, centre.y - rad - cs * 0.4 - (lines - 1.0) * cs * 1.2);
                        let t = b.label(Role::Category, at, cat, Justify::Center, false);
                        on_top.push(t);
                    } else {
                        let t = b.label(Role::Category, Point::new(centre.x, centre.y + rad + cs * 1.6), cat, Justify::Center, false);
                        b.out.push(t);
                    }
                }
            }
        }
        GraphKind::Radar => {
            let centre = r.center();
            let rad = (r.width().min(r.height()) / 2.0).max(1.0);
            let (lo, hi, step) = value_axis(g.ticks, g.axis_min, g.axis_max, 0.0, g.rows.iter().flatten().copied().fold(0.0, f64::max));
            let at = |c: usize, v: f64| {
                let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * c as f64 / ncat as f64;
                centre + vectorcraft_geom::Vec2::new(a.cos(), a.sin()) * (rad * ((v - lo) / (hi - lo)).clamp(0.0, 1.0))
            };
            let mut axes = vec![];
            for c in 0..ncat {
                axes.push(b.line(centre, at(c, hi)));
                if let Some(cat) = g.categories.get(c).filter(|c| !c.is_empty()) {
                    let p = at(c, hi) + (at(c, hi) - centre).normalize() * (cs * 1.2);
                    axes.push(b.label(Role::Category, Point::new(p.x, p.y + cs * 0.35), cat, Justify::Center, true));
                }
            }
            for v in tick_values(lo, hi, step).into_iter().skip(1) {
                let ring: Vec<Point> = (0..ncat).map(|c| at(c, v)).collect();
                axes.push(b.path(polyline(&ring, true), Paint::None, Paint::solid(Color::rgb(0.6, 0.6, 0.6)), 0.25));
            }
            let ax = b.group("Axes", axes);
            b.out.push(ax);
            for (s, items) in series.iter_mut().enumerate() {
                let pts: Vec<Point> = (0..ncat).map(|c| at(c, val(c, s))).collect();
                if g.connect_points && pts.len() > 1 {
                    let n = b.data_line(g, s, polyline(&pts, true), filled);
                    shadowed.push((s, n.clone()));
                    items.push(n);
                }
                if g.mark_points {
                    for p in pts {
                        let paint = mark(s, true, Paint::None, 0.0);
                        items.push(b.marker(design(s), Rect::from_center_size(p, (4.0, 4.0)), paint));
                    }
                }
            }
        }
        _ => {
            let scatter = g.kind == GraphKind::Scatter;
            // Separate Scales: with the value axis on both sides, the series assigned to the right axis get a scale
            // of their own there (axis 1); every other series, and every series without separate scales, uses axis 0.
            let separate = g.separate_scales && g.value_axis == ValueAxisSide::Both && !horizontal && !scatter;
            let mut on_right = vec![false; nser];
            if separate {
                for i in &g.right_series {
                    if let Some(x) = on_right.get_mut(*i) {
                        *x = true;
                    }
                }
            }
            let axis = |s: usize| usize::from(on_right.get(s).copied().unwrap_or(false));
            // Value range per axis (stacked graphs stack positives and negatives separately).
            let (mut lo, mut hi) = ([0.0f64; 2], [0.0f64; 2]);
            let (mut xlo, mut xhi) = (f64::MAX, f64::MIN);
            for c in 0..ncat {
                if !scatter {
                    for (a, (lo, hi)) in lo.iter_mut().zip(hi.iter_mut()).enumerate() {
                        // Stacked columns (or bars) stack together, and so do areas; other series count one value each.
                        for stack in [[GraphKind::StackedColumn, GraphKind::StackedBar], [GraphKind::Area, GraphKind::Area]] {
                            let members = || (0..nser).filter(|s| axis(*s) == a && stack.contains(&kind(*s)));
                            *hi = hi.max(members().map(|s| val(c, s).max(0.0)).sum());
                            *lo = lo.min(members().map(|s| val(c, s).min(0.0)).sum());
                        }
                        let single = |s: &usize| axis(*s) == a && matches!(kind(*s), GraphKind::Column | GraphKind::Bar | GraphKind::Line);
                        for v in (0..nser).filter(single).filter_map(|s| cell(c, s)) {
                            *hi = hi.max(v);
                            *lo = lo.min(v);
                        }
                    }
                } else {
                    for s in (0..nser).step_by(2) {
                        if let (Some(y), Some(x)) = (cell(c, s), cell(c, s + 1)) {
                            hi[0] = hi[0].max(y);
                            lo[0] = lo[0].min(y);
                            xlo = xlo.min(x);
                            xhi = xhi.max(x);
                        }
                    }
                }
            }
            // An axis with no series of its own shows the other one's scale.
            let left = value_axis(g.ticks, g.axis_min, g.axis_max, lo[0], hi[0]);
            let used = |a: usize| (0..nser).any(|s| axis(s) == a);
            let (left, right) = if !separate || !used(1) {
                (left, left)
            } else {
                let right = value_axis(g.right_ticks, g.right_axis_min, g.right_axis_max, lo[1], hi[1]);
                if used(0) { (left, right) } else { (right, right) }
            };
            let scale = |a: usize| if a == 1 { right } else { left };
            // Value → coordinate along value axis `a` (y for columns/lines, x for bars), and that axis' zero (or the
            // end nearest it).
            let vpos = |a: usize, v: f64| {
                let (lo, hi, _) = scale(a);
                let t = (v - lo) / (hi - lo);
                if horizontal { r.x0 + t * r.width() } else { r.y1 - t * r.height() }
            };
            let zero = |a: usize| {
                let (lo, hi, _) = scale(a);
                vpos(a, 0.0f64.clamp(lo, hi))
            };
            // Plot points per unit of value along value axis `a`.
            let per_unit = |a: usize| {
                let (lo, hi, _) = scale(a);
                (if horizontal { r.width() } else { r.height() }) / (hi - lo)
            };
            let mut axes = vec![];
            // Value axes with ticks and labels: along the bottom for bar graphs, else on the chosen sides (the right
            // one shows the right scale).
            let (on_left_side, on_right_side) =
                if horizontal { (true, false) } else { (g.value_axis != ValueAxisSide::Right, g.value_axis != ValueAxisSide::Left) };
            // Add Labels text per axis, kept to one short line whatever the file holds.
            let affix = |s: &str| one_line(s);
            let affixes = [(affix(&g.prefix), affix(&g.suffix)), (affix(&g.right_prefix), affix(&g.right_suffix))];
            // Each label's tick mark, then the extra tick marks of its division (Tick Marks per division).
            let value_ticks = |b: &mut Gen, axes: &mut Vec<Arc<Node>>, a: usize, v: f64, step: f64, hi: f64| {
                let (len, marks) = if a == 1 { (g.right_tick_length, g.right_tick_marks) } else { (g.tick_length, g.tick_marks) };
                let n = marks.clamp(1, MAX_TICK_MARKS);
                for k in 0..n {
                    let w = v + step * k as f64 / n as f64;
                    if k > 0 && w >= hi - step * 1e-9 {
                        break;
                    }
                    let q = vpos(a, w);
                    // A full-length tick on the zero line, or repeating the left side's on one shared scale, adds nothing.
                    let repeat = a == 1 && on_left_side && g.tick_length == TickLength::Full && right == left;
                    let len = if len == TickLength::Full && ((q - zero(a)).abs() < 1e-9 || repeat) { TickLength::None } else { len };
                    let ends = match (len, horizontal, a == 1) {
                        (TickLength::None, ..) => None,
                        (TickLength::Short, true, _) => Some((Point::new(q, r.y1), Point::new(q, r.y1 + 4.0))),
                        (TickLength::Short, false, false) => Some((Point::new(r.x0 - 4.0, q), Point::new(r.x0, q))),
                        (TickLength::Short, false, true) => Some((Point::new(r.x1, q), Point::new(r.x1 + 4.0, q))),
                        (TickLength::Full, true, _) => Some((Point::new(q, r.y0), Point::new(q, r.y1))),
                        (TickLength::Full, false, _) => Some((Point::new(r.x0, q), Point::new(r.x1, q))),
                    };
                    if let Some((p0, p1)) = ends {
                        axes.push(b.line(p0, p1));
                    }
                    if k == 0 {
                        // The label goes right after its own tick mark.
                        let (prefix, suffix) = affixes.get(a).map_or(("", ""), |(p, s)| (p.as_str(), s.as_str()));
                        let label = format!("{prefix}{}{suffix}", fmt_value(v));
                        let t = if horizontal {
                            b.text(Role::Value, Point::new(q, r.y1 + 4.0 + vs * 1.1), &label, Justify::Center)
                        } else if a == 1 {
                            b.text(Role::Value, Point::new(r.x1 + 6.0, q + vs * 0.35), &label, Justify::Left)
                        } else {
                            b.text(Role::Value, Point::new(r.x0 - 6.0, q + vs * 0.35), &label, Justify::Right)
                        };
                        axes.push(t);
                    }
                }
            };
            if on_left_side {
                let (lo, hi, step) = left;
                for v in tick_values(lo, hi, step) {
                    value_ticks(&mut b, &mut axes, 0, v, step, hi);
                }
            }
            if on_right_side {
                let (lo, hi, step) = right;
                for v in tick_values(lo, hi, step) {
                    value_ticks(&mut b, &mut axes, 1, v, step, hi);
                }
            }
            // The legend starts after the value labels right of the plot: the right axis' (14 points on), or a bar
            // graph's last bottom label running past the plot (6 points on; a short number stays within the gap).
            for t in &axes {
                if matches!(t.kind, NodeKind::Text(_))
                    && let Some(bb) = t.geometric_bounds()
                    && bb.x1 > r.x1
                {
                    right_edge = right_edge.max(if bb.x0 > r.x1 { bb.x1 } else { bb.x1 - 8.0 });
                }
            }
            if horizontal {
                axes.push(b.line(Point::new(r.x0, r.y1), Point::new(r.x1, r.y1)));
                axes.push(b.line(Point::new(zero(0), r.y0), Point::new(zero(0), r.y1)));
            } else {
                if on_left_side {
                    axes.push(b.line(Point::new(r.x0, r.y0), Point::new(r.x0, r.y1)));
                }
                if on_right_side {
                    axes.push(b.line(Point::new(r.x1, r.y0), Point::new(r.x1, r.y1)));
                }
                axes.push(b.line(Point::new(r.x0, zero(0)), Point::new(r.x1, zero(0))));
                // The right scale's zero, where its columns start, when it isn't the left one's.
                if (zero(1) - zero(0)).abs() > 1e-9 {
                    axes.push(b.line(Point::new(r.x0, zero(1)), Point::new(r.x1, zero(1))));
                }
            }
            // Category positions.
            let span = if horizontal { r.height() } else { r.width() };
            let cat_w = span / ncat as f64;
            let cat_start = |c: usize| if horizontal { r.y0 + c as f64 * cat_w } else { r.x0 + c as f64 * cat_w };
            // Area bands always span the plot; lines only with Edge-to-Edge Lines. Category labels sit where every
            // series puts its points: at the edges when all of them span the plot, else at the category centres.
            let spans = |k: GraphKind| k == GraphKind::Area || (k == GraphKind::Line && g.edge_to_edge);
            let mid = |c: usize, edges: bool| {
                if edges && ncat > 1 { r.x0 + r.width() * c as f64 / (ncat - 1) as f64 } else { cat_start(c) + cat_w / 2.0 }
            };
            let label_edges = kinds.iter().all(|k| spans(*k));
            let cat_mid = |c: usize| mid(c, label_edges);
            if !scatter {
                for c in 0..ncat {
                    if let Some(cat) = g.categories.get(c).filter(|c| !c.is_empty()) {
                        let t = if horizontal {
                            b.label(Role::Category, Point::new(r.x0 - 6.0, cat_mid(c) + cs * 0.35), cat, Justify::Right, true)
                        } else {
                            b.label(Role::Category, Point::new(cat_mid(c), r.y1 + cs * 1.4), cat, Justify::Center, false)
                        };
                        axes.push(t);
                    }
                }
                // Category tick marks (none by default): at the labels, or between them (at the category edges),
                // with the extra tick marks of each division between those.
                let len = g.category_tick_length;
                if len != TickLength::None {
                    let shown = ncat.min(MAX_CATEGORY_TICKS);
                    let base: Vec<f64> = match (g.ticks_between_labels, label_edges) {
                        // Labels at the plot's edges: halfway between neighbouring labels.
                        (true, true) => (1..shown).map(|c| (cat_mid(c - 1) + cat_mid(c)) / 2.0).collect(),
                        (true, false) => (0..=shown).map(cat_start).collect(),
                        (false, _) => (0..shown).map(cat_mid).collect(),
                    };
                    // Fewer per division on a long axis, so the cap still ticks all of it.
                    let n = g.category_tick_marks.clamp(1, MAX_TICK_MARKS).min(MAX_CATEGORY_TICKS / base.len().max(1)).max(1);
                    let at = base.iter().enumerate().flat_map(|(i, q)| {
                        let next = base.get(i + 1).copied();
                        (0..n).filter_map(move |k| if k == 0 { Some(*q) } else { next.map(|x| q + (x - q) * k as f64 / n as f64) })
                    });
                    // A full-length tick on a drawn axis line adds nothing.
                    let on_axis_line = |q: f64| {
                        if horizontal {
                            (q - r.y1).abs() < 1e-9
                        } else {
                            ((q - r.x0).abs() < 1e-9 && on_left_side) || ((q - r.x1).abs() < 1e-9 && on_right_side)
                        }
                    };
                    for q in at.take(MAX_CATEGORY_TICKS) {
                        if len == TickLength::Full && on_axis_line(q) {
                            continue;
                        }
                        let (p0, p1) = match (len, horizontal) {
                            (TickLength::Full, false) => (Point::new(q, r.y0), Point::new(q, r.y1)),
                            (TickLength::Full, true) => (Point::new(r.x0, q), Point::new(r.x1, q)),
                            (_, false) => (Point::new(q, r.y1), Point::new(q, r.y1 + 4.0)),
                            (_, true) => (Point::new(r.x0 - 4.0, q), Point::new(r.x0, q)),
                        };
                        axes.push(b.line(p0, p1));
                    }
                }
            } else {
                // Scatter: a horizontal value axis too.
                // Like the value axis: finite, and a counted (capped) list of ticks, so data near f64::MAX can't loop forever.
                let x_lo = if xlo == f64::MAX { 0.0 } else { xlo.min(0.0) };
                let x_hi = if xhi == f64::MIN { 1.0 } else { xhi };
                let (xl, xh, xs) = value_axis(g.ticks, None, None, x_lo, x_hi);
                for x in tick_values(xl, xh, xs) {
                    let q = r.x0 + (x - xl) / (xh - xl) * r.width();
                    axes.push(b.line(Point::new(q, r.y1), Point::new(q, r.y1 + 4.0)));
                    axes.push(b.text(Role::Value, Point::new(q, r.y1 + 4.0 + vs * 1.1), &fmt_value(x), Justify::Center));
                }
                for (si, s) in (0..nser).step_by(2).enumerate() {
                    let pts: Vec<Option<Point>> = (0..ncat)
                        .map(|c| match (cell(c, s), cell(c, s + 1)) {
                            (Some(y), Some(x)) => Some(Point::new(r.x0 + (x - xl) / (xh - xl) * r.width(), vpos(0, y))),
                            _ => None,
                        })
                        .collect();
                    if g.connect_points {
                        for run in runs(&pts) {
                            let n = b.data_line(g, si, polyline(&run, false), filled);
                            shadowed.push((si, n.clone()));
                            series[si].push(n);
                        }
                    }
                    if g.mark_points {
                        for p in pts.iter().flatten() {
                            let paint = mark(si, true, Paint::None, 0.0);
                            let n = b.marker(design(si), Rect::from_center_size(*p, (5.0, 5.0)), paint);
                            series[si].push(n);
                        }
                    }
                }
            }
            let ax = b.group("Axes", axes);
            b.out.push(ax);
            // Columns (or bars) of one category side by side; stacked columns take one slot of that cluster, or, with
            // no plain columns, a width of their own.
            let columns: Vec<usize> = (0..nser).filter(|s| matches!(kind(*s), GraphKind::Column | GraphKind::Bar)).collect();
            let stacks: Vec<usize> = (0..nser).filter(|s| matches!(kind(*s), GraphKind::StackedColumn | GraphKind::StackedBar)).collect();
            // One stack per value axis that has stacked series, each in a slot of its own.
            let stack_axes: Vec<usize> = (0..2).filter(|a| stacks.iter().any(|s| axis(*s) == *a)).collect();
            let own_width = columns.is_empty() && stack_axes.len() < 2;
            let slots = columns.len() + stack_axes.len();
            // Widths past 100% make the columns (or clusters) overlap.
            let cluster = cat_w * (g.cluster_width / 100.0).clamp(0.01, 10.0);
            let slot = cluster / slots.max(1) as f64;
            let bar = slot * (g.column_width / 100.0).clamp(0.01, 10.0);
            let bar_rect = |a: f64, w: f64, v0: f64, v1: f64| {
                if horizontal { Rect::new(v0.min(v1), a, v0.max(v1), a + w) } else { Rect::new(a, v0.min(v1), a + w, v0.max(v1)) }
            };
            // First Row in Front: the first category drawn last, over the next.
            let cats: Vec<usize> = if g.first_row_in_front { (0..ncat).rev().collect() } else { (0..ncat).collect() };
            for c in cats {
                let c0 = cat_start(c) + (cat_w - cluster) / 2.0;
                for (i, &s) in columns.iter().enumerate() {
                    let Some(v) = cell(c, s) else { continue };
                    let a = c0 + slot * i as f64 + (slot - bar) / 2.0;
                    let (fill, stroke, w) = mark(s, true, Paint::None, 0.0);
                    let (v0, v1) = (zero(axis(s)), vpos(axis(s), v));
                    let rect = bar_rect(a, bar, v0, v1);
                    bars_reach = Some(bars_reach.map_or(rect, |r| r.union(rect)));
                    // A column drawn with a design casts no shadow.
                    let n = match column_look(s).and_then(|c| column_art(b.d, c, (rect, v), (horizontal, v1 < v0), per_unit(axis(s)), &mut b.columns))
                    {
                        Some(n) => {
                            // A design can reach past its column (Uniformly Scaled), so the legend keeps clear of it too.
                            if let Some(art) = n.geometric_bounds() {
                                bars_reach = Some(bars_reach.map_or(art, |r| r.union(art)));
                            }
                            n
                        }
                        None => {
                            let n = b.path(shapes::rectangle(rect), fill, stroke, w);
                            shadowed.push((s, n.clone()));
                            n
                        }
                    };
                    if let Some(items) = series.get_mut(s) {
                        items.push(n);
                    }
                }
                let place = |ax: usize| {
                    if own_width {
                        // Up to the category's width, or past it when the widths are set past 100%.
                        let (col, clus) = ((g.column_width / 100.0).clamp(0.01, 10.0), (g.cluster_width / 100.0).clamp(0.01, 10.0));
                        let w = (cat_w * col * clus * 1.2).min(cat_w * (col * clus).max(1.0));
                        (cat_start(c) + (cat_w - w) / 2.0, w)
                    } else {
                        let i = columns.len() + stack_axes.iter().position(|a| *a == ax).unwrap_or(0);
                        (c0 + slot * i as f64 + (slot - bar) / 2.0, bar)
                    }
                };
                // Each axis stacks its own series.
                let (mut pos, mut neg) = ([0.0f64; 2], [0.0f64; 2]);
                for &s in &stacks {
                    let Some(v) = cell(c, s) else { continue };
                    let ax = axis(s);
                    let (a, w) = place(ax);
                    let Some(base) = (if v >= 0.0 { pos.get_mut(ax) } else { neg.get_mut(ax) }) else { continue };
                    let (v0, v1) = (vpos(ax, *base), vpos(ax, *base + v));
                    *base += v;
                    let (fill, stroke, sw) = mark(s, true, Paint::solid(Color::WHITE), 0.25);
                    let rect = bar_rect(a, w, v0, v1);
                    bars_reach = Some(bars_reach.map_or(rect, |r| r.union(rect)));
                    let n = match column_look(s).and_then(|c| column_art(b.d, c, (rect, v), (horizontal, v1 < v0), per_unit(ax), &mut b.columns)) {
                        Some(n) => {
                            if let Some(art) = n.geometric_bounds() {
                                bars_reach = Some(bars_reach.map_or(art, |r| r.union(art)));
                            }
                            n
                        }
                        None => {
                            let n = b.path(shapes::rectangle(rect), fill, stroke, sw);
                            // A segment of 0 has no area to shade.
                            if v != 0.0 {
                                shadowed.push((s, n.clone()));
                            }
                            n
                        }
                    };
                    if let Some(items) = series.get_mut(s) {
                        items.push(n);
                    }
                }
            }
            // Cumulative area bands, each from the previous area total on its axis up to its own.
            let mut belows: [Vec<f64>; 2] = [vec![0.0; ncat], vec![0.0; ncat]];
            for s in (0..nser).filter(|s| kind(*s) == GraphKind::Area) {
                let a = axis(s);
                let Some(below) = belows.get_mut(a) else { continue };
                let above: Vec<f64> = below.iter().enumerate().map(|(c, b)| b + val(c, s)).collect();
                // Mixed with series at the category centres, areas take their points there too.
                let at = |c: usize, v: &[f64]| Point::new(mid(c, label_edges), vpos(a, v.get(c).copied().unwrap_or(0.0)));
                let mut pts: Vec<Point> = (0..ncat).map(|c| at(c, &above)).collect();
                pts.extend((0..ncat).rev().map(|c| at(c, below)));
                let (fill, stroke, w) = mark(s, true, Paint::solid(Color::WHITE), 0.25);
                let n = b.path(polyline(&pts, true), fill, stroke, w);
                if let Some(items) = series.get_mut(s) {
                    items.push(n);
                }
                *below = above;
            }
            for s in (0..nser).filter(|s| kind(*s) == GraphKind::Line) {
                let pts: Vec<Option<Point>> =
                    (0..ncat).map(|c| cell(c, s).map(|v| Point::new(mid(c, g.edge_to_edge && label_edges), vpos(axis(s), v)))).collect();
                if g.connect_points {
                    for run in runs(&pts) {
                        let n = b.data_line(g, s, polyline(&run, false), filled);
                        shadowed.push((s, n.clone()));
                        if let Some(items) = series.get_mut(s) {
                            items.push(n);
                        }
                    }
                }
                if g.mark_points {
                    for p in pts.iter().flatten() {
                        let paint = mark(s, true, Paint::None, 0.0);
                        let n = b.marker(design(s), Rect::from_center_size(*p, (5.0, 5.0)), paint);
                        if let Some(items) = series.get_mut(s) {
                            items.push(n);
                        }
                    }
                }
            }
        }
    }
    // Legend: a swatch + label per series, right of the plot; the swatch joins its series group.
    let legend_series = if g.kind == GraphKind::Scatter { nser.div_ceil(2) } else { nser };
    let mut labels = vec![];
    let legend_on = g.legend && !g.series.is_empty() && !(g.kind == GraphKind::Pie && g.pie_legend_in_wedges);
    if legend_on && g.legend_across_top {
        // Add Legend Across Top: swatch and label after swatch and label, in rows as wide as the plot, the last row
        // just above it.
        let entries: Vec<(usize, String)> = (0..legend_series)
            .map(|s| (s, if g.kind == GraphKind::Scatter { series_label(s * 2) } else { series_label(s) }))
            .filter(|(_, l)| !blank_label(l))
            .collect();
        let mut rows: Vec<Vec<(usize, String, f64)>> = vec![vec![]];
        let mut used = 0.0;
        for (s, label) in entries {
            let w = 12.0 + label_width(&g.label_styles, &label) + 12.0;
            // The gap after an entry may run past the plot's edge.
            if used + w - 12.0 > r.width() && rows.last().is_some_and(|row| !row.is_empty()) {
                rows.push(vec![]);
                used = 0.0;
            }
            used += w;
            if let Some(row) = rows.last_mut() {
                row.push((s, label, w));
            }
        }
        let height = |row: &[(usize, String, f64)]| ls * 1.8 * row.iter().map(|(_, l, _)| l.split('|').count()).max().unwrap_or(1) as f64;
        // Above the plot and anything drawn over it (radar and stacked-pie labels).
        let top = b.out.iter().chain(on_top.iter()).filter_map(|n| n.geometric_bounds()).map(|bb| bb.y0).fold(r.y0, f64::min);
        let top = bars_reach.map_or(top, |b| top.min(b.y0));
        let mut y = top - 8.0 - rows.iter().map(|row| height(row)).sum::<f64>();
        for row in &rows {
            let mut x = r.x0;
            for (s, label, w) in row {
                let marked = matches!(g.kind, GraphKind::Scatter | GraphKind::Radar) || kind(*s) == GraphKind::Line;
                let paint = mark(*s, true, Paint::None, 0.0);
                let swatch =
                    b.swatch(column_look(*s).filter(|_| columnar(*s)), design(*s).filter(|_| marked), Rect::new(x, y, x + 8.0, y + 8.0), paint);
                if let Some(items) = series.get_mut(*s) {
                    items.push(swatch);
                }
                labels.push(b.label(Role::Legend, Point::new(x + 12.0, y + 7.5 + (ls - LABEL_SIZE) * 0.35), label, Justify::Left, false));
                x += w;
            }
            y += height(row);
        }
    } else if legend_on {
        let x = bars_reach.map_or(right_edge, |b| right_edge.max(b.x1)) + 14.0;
        // Each row as tall as its label's lines (a `|` in a label starts a new line).
        let mut y = r.y0;
        for (s, items) in series.iter_mut().enumerate().take(legend_series) {
            let label = if g.kind == GraphKind::Scatter { series_label(s * 2) } else { series_label(s) };
            let row_y = y;
            if blank_label(&label) {
                y += ls * 1.8;
                continue;
            }
            y += ls * 1.8 * label.split('|').count() as f64;
            let y = row_y;
            // Series drawn with markers show their marker design in the legend, and series drawn with a column design
            // that design.
            let marked = matches!(g.kind, GraphKind::Scatter | GraphKind::Radar) || kind(s) == GraphKind::Line;
            let paint = mark(s, true, Paint::None, 0.0);
            items.push(b.swatch(column_look(s).filter(|_| columnar(s)), design(s).filter(|_| marked), Rect::new(x, y, x + 8.0, y + 8.0), paint));
            labels.push(b.label(Role::Legend, Point::new(x + 12.0, y + 7.5 + (ls - LABEL_SIZE) * 0.35), &label, Justify::Left, false));
        }
    }
    // Areas at the back, lines and points in front of columns; one graph type keeps the series order.
    let depth = |s: usize| match kind(s) {
        GraphKind::Area => 0,
        GraphKind::Line => 2,
        _ => 1,
    };
    // Add Drop Shadow: a translucent black copy of each column, bar, line and wedge, a little down and right, just
    // behind the series of its layer (areas, columns, lines), so a layer in front doesn't hide the shadows of the one
    // behind. Left out past SHADOW_BUDGET marks.
    if !g.drop_shadow || shadowed.len() > SHADOW_BUDGET {
        shadowed.clear();
    }
    let mut series: Vec<(usize, Vec<Arc<Node>>)> = series.into_iter().enumerate().collect();
    // First Column in Front: the first series drawn last in its layer, over the others.
    if g.first_column_in_front {
        series.reverse();
    }
    series.sort_by_key(|(s, _)| depth(*s));
    let mut layer = None;
    for (s, items) in series {
        if items.is_empty() {
            continue;
        }
        if layer != Some(depth(s)) {
            layer = Some(depth(s));
            let copies: Vec<Arc<Node>> = shadowed.iter().filter(|(t, _)| depth(*t) == depth(s)).map(|(_, n)| shadow_of(b.d, n)).collect();
            if !copies.is_empty() {
                let mut grp = Node::group(b.d.alloc_id(), copies);
                grp.name = Some("Drop Shadow".into());
                grp.opacity = SHADOW_OPACITY;
                b.out.push(Arc::new(grp));
            }
        }
        let name = g.series.get(s).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Series {}", s + 1));
        let grp = b.series(&name, s, items);
        b.out.push(grp);
    }
    b.out.append(&mut on_top);
    labels.append(&mut wedge_labels);
    if !labels.is_empty() {
        let grp = b.group("Legend", labels);
        b.out.push(grp);
    }
    (!b.columns.short).then_some(b.out)
}

fn children_bounds(children: &[Arc<Node>]) -> Option<Rect> {
    children.iter().filter_map(|c| c.geometric_bounds()).reduce(|a, b| a.union(b))
}

/// Regenerate graph `id` from `spec` (adjusting the plot rect for any move/scale since the last
/// generation, which is recorded in the spec's `placed` bounds).
fn regenerate(d: &mut Document, id: NodeId, mut spec: GraphSpec) -> Result<()> {
    let n = d.node(id).ok_or(EngineError::NoNode(id))?;
    if let (Some(placed), Some(now)) = (n.extras().graph.as_ref().and_then(|g| g.placed), n.children().and_then(|c| children_bounds(c)))
        && placed.width() > 1e-9
        && placed.height() > 1e-9
        && (placed.x0 - now.x0).abs() + (placed.y0 - now.y0).abs() + (placed.x1 - now.x1).abs() + (placed.y1 - now.y1).abs() > 1e-6
    {
        let a = Affine::translate(now.origin().to_vec2())
            * Affine::scale_non_uniform(now.width() / placed.width(), now.height() / placed.height())
            * Affine::translate(-placed.origin().to_vec2());
        spec.rect = a.transform_rect_bbox(spec.rect);
    }
    let children = generate(d, &spec);
    spec.placed = children_bounds(&children);
    let n = d.node_mut(id).ok_or(EngineError::NoNode(id))?;
    if let Some(ch) = n.children_mut() {
        *ch = children;
    }
    n.extras_mut().graph = Some(Box::new(spec));
    Ok(())
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.create";
    let kind =
        str_param(p, "type").map(|t| GraphKind::parse(t).ok_or_else(|| bad(C, format!("unknown graph type `{t}`")))).transpose()?.unwrap_or_default();
    let (x, y) = (f64_req(p, "x", C)?, f64_req(p, "y", C)?);
    let (w, h) = (f64_or(p, "width", 200.0).abs().max(1.0), f64_or(p, "height", 150.0).abs().max(1.0));
    let mut spec = GraphSpec { kind, rect: Rect::new(x, y, x + w, y + h), ..GraphSpec::default() };
    if !apply_data(&mut spec, p) {
        // Starter data so the new graph shows something (edit it with Object → Graph → Data…).
        spec.series = vec!["Series 1".into(), "Series 2".into()];
        spec.categories = vec!["A".into(), "B".into(), "C".into(), "D".into()];
        spec.rows = vec![vec![3.0, 2.0], vec![5.0, 4.0], vec![4.0, 6.0], vec![7.0, 5.0]];
    }
    let parent = s.doc()?.target_parent()?;
    let id = s.edit("Graph", |d, sel| {
        let gid = d.alloc_id();
        let mut g = Node::group(gid, vec![]);
        g.name = Some(format!("{} Graph", kind.label()));
        d.insert(parent, usize::MAX, g)?;
        regenerate(d, gid, spec)?;
        sel.set([gid]);
        Ok(gid)
    })?;
    Ok(json!({ "id": id.0 }))
}

/// The selected graph (or the graph containing the selection).
fn target(s: &Session, p: &Value, cmd: &str) -> Result<NodeId> {
    let st = s.doc()?;
    let start = id_param(p, "id").into_iter().chain(st.selection.objects.iter().copied());
    for id in start {
        let mut cur = Some(id);
        while let Some(c) = cur {
            if st.doc.node(c).is_some_and(|n| n.extras().graph.is_some()) {
                return Ok(c);
            }
            cur = st.doc.parent_of(c);
        }
    }
    Err(bad(cmd, "select a graph"))
}

fn spec_of(s: &Session, id: NodeId) -> Result<GraphSpec> {
    s.doc()?.doc.node(id).and_then(|n| n.extras().graph.as_deref().cloned()).ok_or(EngineError::NoNode(id))
}

/// Where a node inside a graph's generated series art belongs: the graph, the series group and
/// the series index stored on that group.
#[derive(Clone, Copy)]
struct SeriesRef {
    graph: NodeId,
    group: NodeId,
    index: usize,
}

/// Every node in a graph's series groups (the groups too), found in one pass over the document.
/// Axes, the legend and groups whose index is past the graph's series (the index comes from the
/// file) are left out.
fn series_members(doc: &Document) -> HashMap<NodeId, SeriesRef> {
    let mut out = HashMap::new();
    doc.walk(|n| {
        let Some(spec) = n.extras().graph.as_deref() else { return };
        let count = spec.rows.iter().map(Vec::len).max().unwrap_or(0).clamp(1, vectorcraft_doc::MAX_GRAPH_SERIES);
        for group in n.children().into_iter().flatten() {
            let Some(index) = group.extras().series_index.and_then(|i| usize::try_from(i).ok()).filter(|i| *i < count) else { continue };
            let r = SeriesRef { graph: n.id, group: group.id, index };
            group.walk(&mut |m| {
                out.insert(m.id, r);
            });
        }
    });
    out
}

/// Group Selection on a graph series targets its constituent marks and legend swatch, not a
/// group-level appearance which the generated art does not inherit. Other ids pass through.
pub(crate) fn paint_targets(doc: &Document, ids: &[NodeId]) -> Vec<NodeId> {
    let members = series_members(doc);
    if members.is_empty() {
        return ids.to_vec();
    }
    let mut out = Vec::new();
    for id in ids {
        // The series' own marks: the art of a marker design inside it keeps the design's paint (painting it would
        // last only until the graph is drawn again), so edit the design to change it.
        match members.get(id) {
            Some(r) if r.group == *id => {
                if let Some(group) = doc.node(*id) {
                    out.extend(group.children().into_iter().flatten().filter(|m| matches!(m.kind, NodeKind::Path { .. })).map(|m| m.id));
                }
            }
            Some(r) if doc.parent_of(*id) != Some(r.group) => {}
            _ => out.push(*id),
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Copy the paint just applied to generated series art into that series' fill or stroke. Called
/// inside the same document edit, so the visible paint and future regeneration undo together.
/// Gradients, patterns and swatch links are kept as [`Paint`], not reduced to a solid colour.
/// Returns whether a series' captured fill or stroke changed.
pub(crate) fn capture_series_paints(doc: &mut Document, ids: &[NodeId], fill: bool) -> bool {
    let members = series_members(doc);
    if members.is_empty() {
        return false;
    }
    let captures: Vec<_> = ids
        .iter()
        .filter_map(|id| {
            // Only a series' own marks set its paint, not the art of a marker design.
            let r = members.get(id).filter(|r| doc.parent_of(*id) == Some(r.group))?;
            let node = doc.node(*id)?;
            let paint = node.appearance.paint_at(None, fill)?.clone();
            let width = (!fill).then(|| node.appearance.stroke_width());
            Some((r.graph, r.index, paint, width))
        })
        .collect();
    let mut changed = false;
    for (graph, index, paint, width) in captures {
        let Some(spec) = doc.node_mut(graph).and_then(|n| n.extras_if_any().and_then(|e| e.graph.as_deref_mut())) else { continue };
        let was = spec.series_paints.clone();
        // `index` is below the graph's series count (`series_members`).
        if spec.series_paints.len() <= index {
            spec.series_paints.resize(index + 1, vectorcraft_doc::SeriesPaint::default());
        }
        let Some(slot) = spec.series_paints.get_mut(index) else { continue };
        if fill {
            slot.fill = Some(paint);
        } else {
            slot.stroke = Some(paint);
            slot.stroke_width = width;
        }
        changed |= spec.series_paints != was;
    }
    changed
}

/// Graph Data › Transpose row/column: categories become series and series categories.
fn transpose(g: &mut GraphSpec) {
    let cells = g.cells();
    let width = cells.iter().map(Vec::len).max().unwrap_or(0);
    let flipped: Vec<Vec<Option<f64>>> = (0..width).map(|s| cells.iter().map(|row| row.get(s).copied().flatten()).collect()).collect();
    std::mem::swap(&mut g.series, &mut g.categories);
    g.set_cells(flipped);
}

/// Graph Data › Switch x/y (scatter graphs): each series' y and x columns trade places, with their labels.
fn switch_xy(g: &mut GraphSpec) {
    let mut cells = g.cells();
    // Labels for every column, so each pair's labels trade places with its data.
    let width = cells.iter().map(Vec::len).max().unwrap_or(0).max(g.series.len());
    g.series.resize(width, String::new());
    for row in &mut cells {
        for pair in row.as_chunks_mut::<2>().0 {
            pair.swap(0, 1);
        }
    }
    for pair in g.series.as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
    while g.series.last().is_some_and(String::is_empty) {
        g.series.pop();
    }
    g.set_cells(cells);
}

/// Every generated label of the document's graphs: its graph and what it shows. Inside a graph's Axes group the
/// value axis numbers come first and the category labels (one per category with a label) last; a scatter graph's
/// are all numbers, a radar graph's all categories. Legend groups hold legend labels, and a pie graph's own texts
/// name its pies.
fn graph_labels(doc: &Document) -> HashMap<NodeId, (NodeId, Role)> {
    let mut out = HashMap::new();
    doc.walk(|n| {
        let Some(g) = n.extras().graph.as_deref() else { return };
        for c in n.children().into_iter().flatten() {
            match (&c.kind, c.name.as_deref(), c.extras().series_index) {
                (NodeKind::Text(_), _, _) if g.kind == GraphKind::Pie => {
                    out.insert(c.id, (n.id, Role::Category));
                }
                (NodeKind::Group { .. }, Some("Legend"), None) => {
                    for t in c.children().into_iter().flatten().filter(|t| matches!(t.kind, NodeKind::Text(_))) {
                        out.insert(t.id, (n.id, Role::Legend));
                    }
                }
                (NodeKind::Group { .. }, Some("Axes"), None) => {
                    let texts: Vec<NodeId> =
                        c.children().into_iter().flatten().filter(|t| matches!(t.kind, NodeKind::Text(_))).map(|t| t.id).collect();
                    let cats = match g.kind {
                        GraphKind::Scatter => 0,
                        GraphKind::Radar => texts.len(),
                        _ => g.categories.iter().take(g.rows.len().max(1)).filter(|c| !c.is_empty()).count().min(texts.len()),
                    };
                    for (i, t) in texts.iter().enumerate() {
                        out.insert(*t, (n.id, if i + cats >= texts.len() { Role::Category } else { Role::Value }));
                    }
                }
                _ => {}
            }
        }
    });
    out
}

/// Keep a formatting change made to graph labels `ids` (Character or Appearance, on the whole graph or on labels
/// picked with Group Selection) for every label of their kind: `change` is applied to the kind's kept style (or
/// the default one), so the labels look the same when the graph is drawn again. Called inside the same edit, so
/// they undo together. Returns whether a kept style changed.
pub(crate) fn capture_label_styles(doc: &mut Document, ids: &[NodeId], change: &dyn Fn(&mut CharStyle)) -> bool {
    if ids.is_empty() {
        return false;
    }
    let labels = graph_labels(doc);
    if labels.is_empty() {
        return false;
    }
    let mut kinds: Vec<(NodeId, Role)> = ids.iter().filter_map(|id| labels.get(id).copied()).collect();
    kinds.sort_by_key(|(g, r)| (g.0, *r as u8));
    kinds.dedup();
    let mut changed = false;
    for (graph, role) in kinds {
        let Some(spec) = doc.node_mut(graph).and_then(|n| n.extras_if_any().and_then(|e| e.graph.as_deref_mut())) else { continue };
        let mut style = label_style(&spec.label_styles, role);
        change(&mut style);
        let slot = match role {
            Role::Value => &mut spec.label_styles.values,
            Role::Category => &mut spec.label_styles.categories,
            Role::Legend => &mut spec.label_styles.legend,
        };
        changed |= slot.as_ref() != Some(&style);
        *slot = Some(style);
    }
    changed
}

fn set_data(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.setData";
    let id = target(s, p, C)?;
    let mut spec = spec_of(s, id)?;
    let flip = bool_or(p, "transpose", false);
    let swap = bool_or(p, "switchXY", false);
    if swap && spec.kind != GraphKind::Scatter {
        return Err(bad(C, "Switch x/y is for scatter graphs"));
    }
    // Transpose and Switch x/y act after any new data in the same call.
    let changed = apply_data(&mut spec, p);
    if flip {
        // The categories become series: at most as many as a graph has series.
        if spec.cells().len() > vectorcraft_doc::MAX_GRAPH_SERIES {
            return Err(bad(
                C,
                format!("a graph has at most {} series, so it transposes with at most that many categories", vectorcraft_doc::MAX_GRAPH_SERIES),
            ));
        }
        transpose(&mut spec);
        drop_empty_rows(&mut spec);
    }
    if swap {
        switch_xy(&mut spec);
    }
    if !(changed || flip || swap) {
        // Blank cells come back as null, so the rows can be edited and sent back as they are.
        return Ok(json!({ "csv": to_csv(&spec), "series": spec.series, "categories": spec.categories, "rows": spec.cells() }));
    }
    // Series assigned to the right axis that the new data no longer has are dropped, so a series added later starts
    // on the left.
    let count = spec.rows.iter().map(Vec::len).max().unwrap_or(0);
    spec.right_series.retain(|i| *i < count);
    s.edit("Graph Data", |d, _| regenerate(d, id, spec))?;
    Ok(json!({ "id": id.0 }))
}

/// The series a command on graph `id` (of `count` series) applies to: `seriesIndexes`, else, with no `id` param, a
/// selection made only of this graph's series (Group Selection). Empty: the whole graph.
fn picked_series(s: &Session, p: &Value, id: NodeId, count: usize, cmd: &str) -> Result<Vec<usize>> {
    Ok(match p.get("seriesIndexes") {
        Some(v) => {
            let a = v.as_array().filter(|a| !a.is_empty()).ok_or_else(|| bad(cmd, "`seriesIndexes` is a non-empty list of series indexes"))?;
            a.iter()
                .map(|i| i.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < count).ok_or_else(|| bad(cmd, format!("no series {i}"))))
                .collect::<Result<_>>()?
        }
        None if p.get("id").is_none() => {
            let st = s.doc()?;
            let members = series_members(&st.doc);
            let picked: Option<Vec<usize>> = st.selection.objects.iter().map(|n| members.get(n).filter(|r| r.graph == id).map(|r| r.index)).collect();
            let mut out = picked.unwrap_or_default();
            out.sort_unstable();
            out.dedup();
            out
        }
        None => vec![],
    })
}

/// Select the series groups `indexes` of graph `id` (they are new nodes after a regeneration).
fn reselect_series(d: &Document, sel: &mut vectorcraft_doc::Selection, id: NodeId, indexes: &[u32]) {
    let groups: Vec<NodeId> = d
        .node(id)
        .and_then(Node::children)
        .into_iter()
        .flatten()
        .filter(|c| c.extras().series_index.is_some_and(|i| indexes.contains(&i)))
        .map(|c| c.id)
        .collect();
    sel.set(groups);
}

/// The document's graph design names, in order.
fn design_names(d: &Document) -> Vec<String> {
    d.graph_designs.iter().map(|x| x.name.clone()).collect()
}

/// Whether graph `g` draws a series with design `name`, as its marker or its columns.
fn draws_with(g: &GraphSpec, name: &str) -> bool {
    g.series_markers.iter().flatten().any(|m| m == name) || g.series_columns.iter().flatten().any(|c| c.design == name)
}

/// Regenerate the graphs whose series use design `name` (it was added or removed).
fn refresh_graphs_using(d: &mut Document, name: &str) -> Result<()> {
    let mut ids = vec![];
    d.walk(|n| {
        if n.extras().graph.as_deref().is_some_and(|g| draws_with(g, name)) {
            ids.push(n.id);
        }
    });
    for id in ids {
        if let Some(spec) = d.node(id).and_then(|n| n.extras().graph.as_deref().cloned()) {
            regenerate(d, id, spec)?;
        }
    }
    Ok(())
}

/// The designs no graph's series draw with (Select Unused).
fn unused_designs(d: &Document) -> Vec<String> {
    let mut used = std::collections::HashSet::new();
    d.walk(|n| {
        if let Some(g) = n.extras().graph.as_deref() {
            used.extend(g.series_markers.iter().flatten().cloned());
            used.extend(g.series_columns.iter().flatten().map(|c| c.design.clone()));
        }
    });
    d.graph_designs.iter().map(|x| x.name.clone()).filter(|n| !used.contains(n)).collect()
}

/// Design names from param `key`: one name or a list of names.
fn names_param(p: &Value, key: &str, cmd: &str) -> Result<Option<Vec<String>>> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::String(n)) => Ok(Some(vec![n.clone()])),
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or_else(|| bad(cmd, format!("`{key}` is a design name or a list of them"))))
            .collect::<Result<_>>()
            .map(Some),
        Some(_) => Err(bad(cmd, format!("`{key}` is a design name or a list of them"))),
    }
}

fn design(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.design";
    for key in ["paste", "rename", "to"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(bad(C, format!("`{key}` is a design name")));
        }
    }
    let taken = |s: &Session, name: &str| s.doc().is_ok_and(|st| st.doc.graph_designs.iter().any(|x| x.name == name));
    // New Design: the selected art, named `new` or "New Design" (numbered when that's taken).
    if let Some(v) = p.get("new") {
        let name = match v {
            Value::String(n) => GraphDesign::clean_name(n).ok_or_else(|| bad(C, "a design needs a name"))?,
            Value::Bool(true) | Value::Null => super::unique_name("New Design", |n| taken(s, n)),
            _ => return Err(bad(C, "`new` is a design name, or true")),
        };
        if taken(s, &name) {
            return Err(bad(C, format!("there is already a design named `{name}`")));
        }
        if s.doc()?.doc.graph_designs.len() >= GraphDesign::MAX {
            return Err(bad(C, format!("a document keeps at most {} graph designs", GraphDesign::MAX)));
        }
        let art = super::brushsym::selection_art(s, p)?.ok_or_else(|| bad(C, "select the art for the design"))?;
        if art.count_with_masks() > GraphDesign::MAX_NODES {
            return Err(bad(C, format!("a design holds at most {} objects", GraphDesign::MAX_NODES)));
        }
        // A graph saved as a design is kept as the art it draws.
        let art = GraphDesign::plain_art(&art);
        let saved = name.clone();
        s.edit("New Design", |d, _| {
            let art = d.reid(&art);
            d.graph_designs.push(GraphDesign { name: saved.clone(), art: Arc::new(art) });
            refresh_graphs_using(d, &saved)
        })?;
        return Ok(json!({ "name": name }));
    }
    // Rename: the graphs drawing the design follow it to its new name.
    if let Some(old) = str_param(p, "rename") {
        if !taken(s, old) {
            return Err(bad(C, format!("no design named `{old}`")));
        }
        let to = str_param(p, "to").and_then(GraphDesign::clean_name).ok_or_else(|| bad(C, "`to` is the design's new name"))?;
        if to == old {
            return Ok(json!({ "name": to }));
        }
        if taken(s, &to) {
            return Err(bad(C, format!("there is already a design named `{to}`")));
        }
        let (old, new) = (old.to_string(), to.clone());
        s.edit("Rename Design", |d, _| {
            if let Some(x) = d.graph_designs.iter_mut().find(|x| x.name == old) {
                x.name = new.clone();
            }
            let mut graphs = vec![];
            d.walk(|n| {
                if n.extras().graph.as_deref().is_some_and(|g| draws_with(g, &old)) {
                    graphs.push(n.id);
                }
            });
            for id in graphs {
                if let Some(g) = d.node_mut(id).and_then(|n| n.extras_if_any().and_then(|e| e.graph.as_deref_mut())) {
                    for m in g.series_markers.iter_mut().flatten().filter(|m| **m == old) {
                        *m = new.clone();
                    }
                    for c in g.series_columns.iter_mut().flatten().filter(|c| c.design == old) {
                        c.design = new.clone();
                    }
                }
            }
            Ok(())
        })?;
        return Ok(json!({ "name": to }));
    }
    if let Some(name) = str_param(p, "paste") {
        let art = s
            .doc()?
            .doc
            .graph_designs
            .iter()
            .find(|x| x.name == name)
            .map(|x| x.art.clone())
            .ok_or_else(|| bad(C, format!("no design named `{name}`")))?;
        let parent = s.doc()?.insertion_parent();
        let id = s.edit("Paste Design", |d, sel| {
            let n = d.reid(&art);
            let id = n.id;
            d.insert(parent, usize::MAX, n)?;
            sel.set([id]);
            Ok(id)
        })?;
        return Ok(json!({ "id": id.0 }));
    }
    if let Some(names) = names_param(p, "delete", C)? {
        if let Some(n) = names.iter().find(|n| !taken(s, n)) {
            return Err(bad(C, format!("no design named `{n}`")));
        }
        s.edit("Delete Design", |d, _| {
            d.graph_designs.retain(|x| !names.contains(&x.name));
            for name in &names {
                refresh_graphs_using(d, name)?;
            }
            Ok(())
        })?;
        return ok();
    }
    // The list, with the designs no graph draws with (Select Unused picks those).
    let d = &s.doc()?.doc;
    Ok(json!({ "designs": design_names(d), "unused": unused_designs(d) }))
}

fn set_marker(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.marker";
    let id = target(s, p, C)?;
    let mut spec = spec_of(s, id)?;
    let count = spec.rows.iter().map(Vec::len).max().unwrap_or(0).min(vectorcraft_doc::MAX_GRAPH_SERIES);
    let picked = picked_series(s, p, id, count, C)?;
    let names = design_names(&s.doc()?.doc);
    let Some(v) = p.get("design") else {
        // The design the picked series (all of them with none picked) share, or null.
        let all: Vec<usize> = if picked.is_empty() { (0..count).collect() } else { picked };
        // A design deleted since doesn't count: those series draw the default marker.
        let used: Vec<Option<&String>> =
            all.iter().map(|i| spec.series_markers.get(*i).and_then(Option::as_ref).filter(|n| names.contains(n))).collect();
        let shared = used.first().copied().flatten().filter(|d| used.iter().all(|x| *x == Some(*d)));
        return Ok(json!({ "designs": names, "design": shared }));
    };
    let design = match v {
        Value::Null => None,
        Value::String(n) if names.contains(n) => Some(n.clone()),
        Value::String(n) => return Err(bad(C, format!("no design named `{n}`"))),
        _ => return Err(bad(C, "`design` is a design name, or null for the default marker")),
    };
    let reselect: Vec<u32> = if p.get("seriesIndexes").is_none() { picked.iter().filter_map(|i| u32::try_from(*i).ok()).collect() } else { vec![] };
    let targets: Vec<usize> = if picked.is_empty() { (0..count).collect() } else { picked };
    spec.series_markers.truncate(count);
    spec.series_markers.resize(count, None);
    for i in targets {
        if let Some(m) = spec.series_markers.get_mut(i) {
            *m = design.clone();
        }
    }
    while spec.series_markers.last().is_some_and(Option::is_none) {
        spec.series_markers.pop();
    }
    if marker_cost(&spec, &marker_designs(&s.doc()?.doc, &spec)) > MARKER_BUDGET {
        return Err(bad(C, "the design is too big to draw at every data point of this graph"));
    }
    s.edit("Graph Marker", |d, sel| {
        regenerate(d, id, spec)?;
        if !reselect.is_empty() {
            reselect_series(d, sel, id, &reselect);
        }
        Ok(())
    })?;
    Ok(json!({ "id": id.0 }))
}

fn set_column(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.column";
    let id = target(s, p, C)?;
    let mut spec = spec_of(s, id)?;
    let count = spec.rows.iter().map(Vec::len).max().unwrap_or(0).min(vectorcraft_doc::MAX_GRAPH_SERIES);
    let picked = picked_series(s, p, id, count, C)?;
    let names = design_names(&s.doc()?.doc);
    let all: Vec<usize> = if picked.is_empty() { (0..count).collect() } else { picked.clone() };
    // The column design the picked series (all of them with none picked) share; a design deleted since doesn't count,
    // as those series draw plain columns.
    let used: Vec<Option<&ColumnDesign>> =
        all.iter().map(|i| spec.series_columns.get(*i).and_then(Option::as_ref).filter(|c| names.contains(&c.design))).collect();
    let shared = used.first().copied().flatten().filter(|c| used.iter().all(|x| *x == Some(*c))).cloned();
    let Some(v) = p.get("design") else {
        let options = shared.clone().unwrap_or_default();
        return Ok(json!({
            "designs": names,
            "design": shared.map(|c| c.design),
            "columnType": options.column_type.id(),
            "each": options.each,
            "fractions": options.fractions.id(),
        }));
    };
    let design = match v {
        Value::Null => None,
        Value::String(n) if names.contains(n) => Some(n.clone()),
        Value::String(n) => return Err(bad(C, format!("no design named `{n}`"))),
        _ => return Err(bad(C, "`design` is a design name, or null for plain columns")),
    };
    let mut options = shared.unwrap_or_default();
    if let Some(t) = p.get("columnType") {
        options.column_type =
            t.as_str().and_then(ColumnType::parse).ok_or_else(|| bad(C, "`columnType` is vertical, uniform, repeating or sliding"))?;
    }
    if let Some(e) = p.get("each") {
        options.each =
            e.as_f64().filter(|e| e.is_finite() && *e > 0.0).ok_or_else(|| bad(C, "`each` is the value one design stands for, more than 0"))?;
    }
    if let Some(f) = p.get("fractions") {
        options.fractions = f.as_str().and_then(Fractions::parse).ok_or_else(|| bad(C, "`fractions` is chop or scale"))?;
    }
    let reselect: Vec<u32> = if p.get("seriesIndexes").is_none() { picked.iter().filter_map(|i| u32::try_from(*i).ok()).collect() } else { vec![] };
    spec.series_columns.truncate(count);
    spec.series_columns.resize(count, None);
    for i in all {
        if let Some(c) = spec.series_columns.get_mut(i) {
            *c = design.clone().map(|design| ColumnDesign { design, ..options.clone() });
        }
    }
    while spec.series_columns.last().is_some_and(Option::is_none) {
        spec.series_columns.pop();
    }
    // Drawn once on a copy of the document, to refuse designs past the budget before the edit.
    let mut probe = (*s.doc()?.doc).clone();
    let looks = column_designs(&probe, &spec);
    if generate_with(&mut probe, &spec, &looks).is_none() {
        return Err(bad(C, "the design is too big to draw in every column of this graph"));
    }
    s.edit("Graph Column", |d, sel| {
        regenerate(d, id, spec)?;
        if !reselect.is_empty() {
            reselect_series(d, sel, id, &reselect);
        }
        Ok(())
    })?;
    Ok(json!({ "id": id.0 }))
}

fn set_type(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "graph.setType";
    let id = target(s, p, C)?;
    let mut spec = spec_of(s, id)?;
    // The series to retype: `seriesIndexes`, else, with no `id`, a selection made only of this graph's series (Group
    // Selection). Anything else selected, the graph itself for one, is the whole graph.
    let count = spec.rows.iter().map(Vec::len).max().unwrap_or(0).min(vectorcraft_doc::MAX_GRAPH_SERIES);
    // A file's list of right-axis series, kept to series the graph has, at most once each.
    spec.right_series.retain(|i| *i < count);
    spec.right_series.sort_unstable();
    spec.right_series.dedup();
    if p.get("seriesIndexes").is_some() && str_param(p, "type").is_none() && str_param(p, "valueAxis").is_none() {
        return Err(bad(C, "`seriesIndexes` needs a `type` or a `valueAxis`"));
    }
    let picked = picked_series(s, p, id, count, C)?;
    let keys = [
        "type",
        "seriesIndexes",
        "columnWidth",
        "clusterWidth",
        "legend",
        "markPoints",
        "connectPoints",
        "edgeToEdge",
        "ticks",
        "axisMin",
        "axisMax",
        "valueAxis",
        "separateScales",
        "rightTicks",
        "rightAxisMin",
        "rightAxisMax",
        "tickLength",
        "tickMarks",
        "rightTickLength",
        "rightTickMarks",
        "categoryTickLength",
        "categoryTickMarks",
        "ticksBetweenLabels",
        "prefix",
        "suffix",
        "rightPrefix",
        "rightSuffix",
        "dropShadow",
        "legendAcrossTop",
        "firstRowInFront",
        "firstColumnInFront",
        "filledLines",
        "pieLegend",
        "piePosition",
        "pieSort",
    ];
    if !keys.iter().any(|k| p.get(*k).is_some()) {
        // With series picked, the type and value axis they share, so the dialog's OK keeps them.
        let kinds: Vec<GraphKind> = picked.iter().map(|i| spec.series_kind(*i)).collect();
        let kind = kinds.first().copied().filter(|k| kinds.iter().all(|x| x == k)).unwrap_or(spec.kind);
        // Series on both axes give no value axis (null), so OK leaves each where it is.
        let sides: Vec<bool> = picked.iter().map(|i| spec.right_series.binary_search(i).is_ok()).collect();
        let value_axis = match sides.first() {
            None => Some(spec.value_axis.id()),
            Some(r) if sides.iter().all(|x| x == r) => Some(if *r { "right" } else { "left" }),
            Some(_) => None,
        };
        let mut fields = json!({
            "type": kind.id(), "columnWidth": spec.column_width, "clusterWidth": spec.cluster_width, "legend": spec.legend,
            "markPoints": spec.mark_points, "connectPoints": spec.connect_points, "edgeToEdge": spec.edge_to_edge, "ticks": spec.ticks,
            "axisMin": spec.axis_min, "axisMax": spec.axis_max, "valueAxis": value_axis, "separateScales": spec.separate_scales,
            "rightTicks": spec.right_ticks, "rightAxisMin": spec.right_axis_min, "rightAxisMax": spec.right_axis_max,
            "tickLength": spec.tick_length.id(), "tickMarks": spec.tick_marks.clamp(1, MAX_TICK_MARKS), "rightTickLength": spec.right_tick_length.id(),
            "rightTickMarks": spec.right_tick_marks.clamp(1, MAX_TICK_MARKS), "categoryTickLength": spec.category_tick_length.id(),
            "categoryTickMarks": spec.category_tick_marks.clamp(1, MAX_TICK_MARKS), "ticksBetweenLabels": spec.ticks_between_labels,
            "prefix": one_line(&spec.prefix), "suffix": one_line(&spec.suffix), "rightPrefix": one_line(&spec.right_prefix),
            "rightSuffix": one_line(&spec.right_suffix), "dropShadow": spec.drop_shadow, "legendAcrossTop": spec.legend_across_top,
            "firstRowInFront": spec.first_row_in_front, "firstColumnInFront": spec.first_column_in_front, "filledLines": spec.filled_lines,
        });
        // Only the options this graph's types use: overlap order for columns and bars, filled lines for lines.
        let kinds: Vec<GraphKind> = (0..count.max(1)).map(|i| spec.series_kind(i)).collect();
        let has = |ks: &[GraphKind]| kinds.iter().any(|k| ks.contains(k));
        if let Some(o) = fields.as_object_mut() {
            if !has(&[GraphKind::Column, GraphKind::StackedColumn, GraphKind::Bar, GraphKind::StackedBar]) {
                o.remove("firstRowInFront");
                o.remove("firstColumnInFront");
            }
            if !has(&[GraphKind::Line, GraphKind::Scatter, GraphKind::Radar]) {
                o.remove("filledLines");
            }
        }
        // Pie graphs: Legend, Position and Sort (their Legend stands for the legend checkbox).
        if let Some(o) = fields.as_object_mut().filter(|_| spec.kind == GraphKind::Pie) {
            o.remove("legend");
            o.insert(
                "pieLegend".into(),
                json!(if !spec.legend {
                    "none"
                } else if spec.pie_legend_in_wedges {
                    "wedges"
                } else {
                    "standard"
                }),
            );
            o.insert("piePosition".into(), json!(spec.pie_position.id()));
            o.insert("pieSort".into(), json!(spec.pie_sort.id()));
        }
        return Ok(fields);
    }
    // Series picked with Group Selection stay selected through the regeneration (their groups are new nodes).
    let reselect: Vec<u32> = if p.get("seriesIndexes").is_none() { picked.iter().filter_map(|i| u32::try_from(*i).ok()).collect() } else { vec![] };
    if let Some(t) = str_param(p, "type") {
        let kind = GraphKind::parse(t).ok_or_else(|| bad(C, format!("unknown graph type `{t}`")))?;
        if picked.is_empty() {
            spec.kind = kind;
        } else {
            if !kind.combines_with(spec.kind) {
                return Err(bad(C, format!("a {} series can't go in a {} graph", kind.label(), spec.kind.label())));
            }
            spec.series_kinds.resize(count, None);
            for &i in &picked {
                if let Some(k) = spec.series_kinds.get_mut(i) {
                    *k = (kind != spec.kind).then_some(kind);
                }
            }
            while spec.series_kinds.last().is_some_and(Option::is_none) {
                spec.series_kinds.pop();
            }
        }
    }
    if let Some(v) = str_param(p, "valueAxis") {
        let side = ValueAxisSide::parse(v).ok_or_else(|| bad(C, format!("unknown value axis `{v}` (left, right or both)")))?;
        if picked.is_empty() {
            spec.value_axis = side;
        } else {
            // A series goes on one axis.
            if side == ValueAxisSide::Both {
                return Err(bad(C, "a series goes on the left or the right value axis"));
            }
            spec.right_series.retain(|i| *i < count && !picked.contains(i));
            if side == ValueAxisSide::Right {
                spec.right_series.extend(&picked);
                spec.right_series.sort_unstable();
            }
        }
    }
    spec.separate_scales = bool_or(p, "separateScales", spec.separate_scales);
    for (key, slot) in [
        ("tickLength", &mut spec.tick_length),
        ("rightTickLength", &mut spec.right_tick_length),
        ("categoryTickLength", &mut spec.category_tick_length),
    ] {
        if let Some(v) = str_param(p, key) {
            *slot = TickLength::parse(v).ok_or_else(|| bad(C, format!("unknown {key} `{v}` (none, short or full)")))?;
        }
    }
    for (key, slot) in
        [("tickMarks", &mut spec.tick_marks), ("rightTickMarks", &mut spec.right_tick_marks), ("categoryTickMarks", &mut spec.category_tick_marks)]
    {
        *slot = count_param(p, key, MAX_TICK_MARKS as u64).map_or(*slot, |n| n.max(1));
    }
    spec.ticks_between_labels = bool_or(p, "ticksBetweenLabels", spec.ticks_between_labels);
    if let Some(v) = str_param(p, "piePosition") {
        spec.pie_position =
            vectorcraft_doc::PiePosition::parse(v).ok_or_else(|| bad(C, format!("unknown piePosition `{v}` (even, ratio or stacked)")))?;
    }
    if let Some(v) = str_param(p, "pieSort") {
        spec.pie_sort = vectorcraft_doc::PieSort::parse(v).ok_or_else(|| bad(C, format!("unknown pieSort `{v}` (none, all or first)")))?;
    }
    for (key, slot) in
        [("prefix", &mut spec.prefix), ("suffix", &mut spec.suffix), ("rightPrefix", &mut spec.right_prefix), ("rightSuffix", &mut spec.right_suffix)]
    {
        if let Some(v) = str_param(p, key) {
            // A label, not a paragraph: one line, at most MAX_AFFIX characters.
            *slot = one_line(v);
        }
    }
    spec.right_ticks = count_param(p, "rightTicks", 100).unwrap_or(spec.right_ticks);
    if let Some(v) = p.get("rightAxisMin") {
        spec.right_axis_min = v.as_f64();
    }
    if let Some(v) = p.get("rightAxisMax") {
        spec.right_axis_max = v.as_f64();
    }
    spec.column_width = f64_or(p, "columnWidth", spec.column_width).clamp(1.0, 1000.0);
    spec.cluster_width = f64_or(p, "clusterWidth", spec.cluster_width).clamp(1.0, 1000.0);
    spec.legend = bool_or(p, "legend", spec.legend);
    spec.drop_shadow = bool_or(p, "dropShadow", spec.drop_shadow);
    spec.first_row_in_front = bool_or(p, "firstRowInFront", spec.first_row_in_front);
    spec.first_column_in_front = bool_or(p, "firstColumnInFront", spec.first_column_in_front);
    spec.filled_lines = bool_or(p, "filledLines", spec.filled_lines);
    spec.legend_across_top = bool_or(p, "legendAcrossTop", spec.legend_across_top);
    // After `legend`, which the Graph Type dialog sends too.
    if let Some(v) = str_param(p, "pieLegend") {
        (spec.legend, spec.pie_legend_in_wedges) = match v.to_ascii_lowercase().as_str() {
            "none" => (false, false),
            "standard" => (true, false),
            "wedges" => (true, true),
            _ => return Err(bad(C, format!("unknown pieLegend `{v}` (none, standard or wedges)"))),
        };
    }
    spec.mark_points = bool_or(p, "markPoints", spec.mark_points);
    spec.connect_points = bool_or(p, "connectPoints", spec.connect_points);
    spec.edge_to_edge = bool_or(p, "edgeToEdge", spec.edge_to_edge);
    spec.ticks = count_param(p, "ticks", 100).unwrap_or(spec.ticks);
    if let Some(v) = p.get("axisMin") {
        spec.axis_min = v.as_f64();
    }
    if let Some(v) = p.get("axisMax") {
        spec.axis_max = v.as_f64();
    }
    let label = format!("{} Graph", spec.kind.label());
    s.edit("Graph Type", |d, sel| {
        regenerate(d, id, spec)?;
        if !reselect.is_empty() {
            reselect_series(d, sel, id, &reselect);
        }
        if let Some(n) = d.node_mut(id)
            && n.name.as_deref().is_some_and(|nm| nm.ends_with(" Graph"))
        {
            n.name = Some(label);
        }
        Ok(())
    })?;
    Ok(json!({ "id": id.0 }))
}

#[cfg(test)]
mod tests;
