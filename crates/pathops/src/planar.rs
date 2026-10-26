//! Live Paint planar maps: the faces and edges a set of paths divides the plane into.
//!
//! Unlike Pathfinder [`regions`](crate::regions), which splits the *filled areas* of closed shapes,
//! every path here is an edge, open ones included: three crossing lines enclose a triangle, a line
//! across a rectangle splits it in two, and an area enclosed by paths is a face whether or not
//! anything fills it.
//!
//! The sweep (`linesweeper`) only takes closed paths, so an open path goes in traced there and
//! back. Its two passes would cancel out as winding numbers, so the sweep counts boundary crossings
//! instead ([`Crossings`]), which never cancel. Faces are then walked on the planar graph of the
//! sweep's output segments; which inputs an edge came from and which fills cover a face are found
//! geometrically (the sweep's winding numbers can't be trusted once open paths and coincident
//! edges are in).
//!
//! The sweep runs in a sheared frame ([`SHEAR`]): it merges the two passes of a *horizontal* open
//! path as if they cancelled out, and in the sheared frame no edge drawn at a common angle (0°,
//! 45°, 90°…) is horizontal. The shear is by a power of two, so it maps typical coordinates
//! exactly and keeps exact coincidences (a corner on another shape's edge) exact.

use std::collections::HashMap;

use kurbo::{Affine, BezPath, CubicBez, ParamCurve, ParamCurveArclen, PathSeg, Point, Shape as _};
use linesweeper::topology::{Topology, WindingNumber};
use vectorcraft_geom::{FillRule, PathData, SubPath};

use crate::boolean::{DEFAULT_PRECISION, Seg, Tidy, eps_for, is_sliver, segs_to_subpath, snap_horizontals, tidy_segments, to_seg};
use crate::pathfinder::{Region, Shape};

/// The sweep's frame: y + x / 32. Only edges sloping -1/32 in the document are horizontal in it.
const SHEAR: Affine = Affine::new([1.0, 1.0 / 32.0, 0.0, 1.0, 0.0, 0.0]);
const UNSHEAR: Affine = Affine::new([1.0, -1.0 / 32.0, 0.0, 1.0, 0.0, 0.0]);

/// Boundary crossings per input, counted from the far west: slot `2i` counts input `i`'s
/// segments running one way, slot `2i + 1` the other. An open path traced there and back adds one
/// to each, so its edges never cancel out of the map. Only the slots that aren't 0 are kept, in
/// order: the sweep keeps one of these for every edge, and a vector as long as the input's number
/// (as this was) made disjoint shapes cost the square of their count in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Crossings(Vec<(usize, u32)>);

impl Crossings {
    fn get(&self, slot: usize) -> u32 {
        self.0.binary_search_by_key(&slot, |(s, _)| *s).map_or(0, |i| self.0.get(i).map_or(0, |(_, c)| *c))
    }
}

impl std::ops::Add for Crossings {
    type Output = Crossings;
    fn add(mut self, rhs: Self) -> Crossings {
        self += rhs;
        self
    }
}

impl std::ops::AddAssign for Crossings {
    fn add_assign(&mut self, rhs: Self) {
        if rhs.0.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(self.0.len() + rhs.0.len());
        let (mut a, mut b) = (std::mem::take(&mut self.0).into_iter().peekable(), rhs.0.into_iter().peekable());
        loop {
            let next = match (a.peek(), b.peek()) {
                (Some(x), Some(y)) if x.0 == y.0 => {
                    let (x, y) = (a.next(), b.next());
                    x.zip(y).map(|(x, y)| (x.0, x.1.saturating_add(y.1)))
                }
                (Some(x), Some(y)) if x.0 < y.0 => a.next(),
                (Some(_), Some(_)) | (None, Some(_)) => b.next(),
                (Some(_), None) => a.next(),
                (None, None) => break,
            };
            out.extend(next.filter(|(_, c)| *c != 0));
        }
        self.0 = out;
    }
}

impl WindingNumber for Crossings {
    type Tag = usize;
    fn single(tag: usize, positive: bool) -> Self {
        Crossings(vec![(2 * tag + usize::from(!positive), 1)])
    }
    fn of_tag(&self, tag: usize) -> Self {
        Crossings([2 * tag, 2 * tag + 1].into_iter().map(|slot| (slot, self.get(slot))).filter(|(_, c)| *c != 0).collect())
    }
}

/// A point in the middle of a run of segments.
fn mid(segs: &[Seg]) -> Option<Point> {
    segs.get(segs.len() / 2).map(|s| s.c.eval(0.5))
}

/// Rectangles looked up by a point inside them, when there are too many to test one by one for
/// every point (the map asks once per edge and once per hole, so a scan of them all made it
/// quadratic in the input, #937). A grid of cells over their bounds, each cell listing the
/// rectangles touching it; a lookup reads the cells around the point. The grid has about one cell
/// per rectangle, so a lookup reads about the rectangles of the point's neighbourhood. A rectangle
/// covering too many cells (a long diagonal's bounds take in most of the art, so an index of the
/// bounds alone scanned a diagonal hatch whole for every point) is filed under the cells of a
/// smaller footprint instead ([`Self::with_footprint`]), or, failing that, scanned for every point.
struct RectIndex<T> {
    items: Vec<(kurbo::Rect, T)>,
    /// The grid's top-left corner, cell size and column count; `cells` lists the items touching
    /// each cell, row by row.
    origin: Point,
    cell: (f64, f64),
    cols: usize,
    cells: Vec<Vec<u32>>,
    /// Items the grid doesn't hold (bounds covering most of it, or not finite), scanned for
    /// every point.
    rest: Vec<usize>,
}

/// The fewest cells one footprint rectangle may always be filed under. The cap is this or two
/// rows or columns of the grid, whichever is more: a thin stripe across the art (one of the faces
/// a hatch cuts a shape into) is filed under the cells along it, while a rectangle covering most
/// of the grid costs more to file and to read than to scan for every point.
const CELLS_PER_FOOTPRINT: usize = 64;
/// The cell references a grid of `cells` cells holds for `n` items before the remaining items go
/// to the scanned list: a few per item, plus a few rows or columns' worth (a long segment is filed
/// under a cell of each row or column it crosses, about `sqrt(cells)` of them in a square grid).
/// Coincident segments make a grid of one column and a row per segment, where each would be filed
/// under every row: a quadratic number of references, which this keeps to `n·sqrt(n)`.
fn reference_budget(n: usize, cells: usize) -> usize {
    let across = (cells as f64).sqrt().ceil().min(1e9) as usize;
    n.saturating_mul(64 + 4 * across).saturating_add(1024)
}

/// The cells (columns or rows) an interval `lo..=hi` touches on one axis of a grid: clamped to the
/// grid, so an interval past its edge gets the nearest cell (whose items `contains` then rejects),
/// and `None` for an interval that isn't a number.
fn cell_span(lo: f64, hi: f64, origin: f64, size: f64, count: usize) -> Option<std::ops::RangeInclusive<usize>> {
    let last = count.saturating_sub(1) as f64;
    let a = ((lo - origin) / size).floor().clamp(0.0, last);
    let b = ((hi - origin) / size).floor().clamp(0.0, last);
    (a.is_finite() && b.is_finite()).then(|| (a as usize)..=(b.max(a) as usize))
}

impl<T> RectIndex<T> {
    fn new(items: Vec<(kurbo::Rect, T)>) -> Self {
        Self::with_footprint(items, |r, _, _| vec![*r])
    }

    /// `footprint(bounds, item, cell side)` gives the rectangles an item is filed under: a lookup
    /// finds the item when its bounds hold the point and one footprint rectangle is within the
    /// lookup's tolerance of it. The bounds themselves (`new`) suit items as compact as their
    /// bounds; a long diagonal segment is better filed under the bounds of its pieces, about a
    /// cell long each, which take in the cells along it rather than every cell its bounds cover.
    fn with_footprint(items: Vec<(kurbo::Rect, T)>, footprint: impl Fn(&kurbo::Rect, &T, f64) -> Vec<kurbo::Rect>) -> Self {
        let n = items.len().max(1);
        let all = items.iter().map(|(r, _)| *r).filter(kurbo::Rect::is_finite).reduce(|a, b| a.union(b)).unwrap_or(kurbo::Rect::ZERO);
        // A cell's side holds one rectangle on average; a grid of collinear rectangles (zero area)
        // is divided along its length.
        let side = (all.area() / n as f64).sqrt();
        let side = if side > 0.0 { side } else { all.width().max(all.height()) / n as f64 }.max(1e-9);
        // Saturating casts: an extent of a billion cells is clamped to the item count, then the
        // grid is kept to a few cells per item however lopsided its bounds.
        let mut cols = ((all.width() / side).ceil() as usize).clamp(1, 2 * n);
        let mut rows = ((all.height() / side).ceil() as usize).clamp(1, 2 * n);
        while cols * rows > 4 * n + 4 {
            if cols > rows { cols = cols.div_ceil(2) } else { rows = rows.div_ceil(2) }
        }
        let cell = ((all.width() / cols as f64).max(1e-9), (all.height() / rows as f64).max(1e-9));
        let cap = CELLS_PER_FOOTPRINT.max(2 * cols.max(rows));
        let mut budget = reference_budget(n, cols * rows);
        let mut cells: Vec<Vec<u32>> = vec![Vec::new(); cols * rows];
        let mut rest = Vec::new();
        for (i, (r, t)) in items.iter().enumerate() {
            // Cells hold `u32` indices to halve the memory of a long stripe filed under a row of them.
            let Ok(index) = u32::try_from(i) else {
                rest.push(i);
                continue;
            };
            let mut scan = !r.is_finite();
            let mut spans = Vec::new();
            for f in if scan { Vec::new() } else { footprint(r, t, cell.0.min(cell.1)) } {
                match (cell_span(f.x0, f.x1, all.x0, cell.0, cols), cell_span(f.y0, f.y1, all.y0, cell.1, rows)) {
                    (Some(xs), Some(ys)) if (xs.end() - xs.start() + 1) * (ys.end() - ys.start() + 1) <= cap => spans.push((xs, ys)),
                    _ => scan = true,
                }
            }
            // The item's cells, counted before they are filed: past the grid's budget the item,
            // and the ones after it, are scanned instead (the index stays bounded in memory).
            // Pieces of one segment that fall in the same cell (a degenerate grid's tiny cells)
            // count and file it once: the pieces come in order, so the repeats are consecutive.
            let mut mine: Vec<usize> = if scan {
                Vec::new()
            } else {
                spans.iter().flat_map(|(xs, ys)| ys.clone().flat_map(|y| xs.clone().map(move |x| y * cols + x))).collect()
            };
            mine.dedup();
            if !scan && mine.len() > budget {
                scan = true;
            }
            if scan {
                rest.push(i);
                continue;
            }
            budget -= mine.len();
            for c in mine {
                if let Some(c) = cells.get_mut(c)
                    && c.last() != Some(&index)
                {
                    c.push(index);
                }
            }
        }
        Self { items, origin: all.origin(), cell, cols, cells, rest }
    }

    /// The items whose rectangle, grown by `tol`, holds `p` (and, with a footprint, one of whose
    /// footprint rectangles is within `tol` of it).
    fn at(&self, p: Point, tol: f64) -> impl Iterator<Item = &T> {
        let mut found = self.rest.clone();
        let rows = self.cells.len() / self.cols.max(1);
        if let (Some(xs), Some(ys)) = (
            cell_span(p.x - tol, p.x + tol, self.origin.x, self.cell.0, self.cols),
            cell_span(p.y - tol, p.y + tol, self.origin.y, self.cell.1, rows),
        ) {
            for y in ys {
                for x in xs.clone() {
                    found.extend(self.cells.get(y * self.cols + x).into_iter().flatten().map(|&i| i as usize));
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found.into_iter().filter_map(move |i| self.items.get(i)).filter(move |(r, _)| r.inflate(tol, tol).contains(p)).map(|(_, t)| t)
    }
}

/// A segment's footprint in a [`RectIndex`]: the bounds of its pieces about a cell long, so a long
/// diagonal is filed under the cells along it, not under every cell its bounds cover.
fn segment_footprint(bounds: &kurbo::Rect, seg: &PathSeg, cell: f64) -> Vec<kurbo::Rect> {
    let pieces = (bounds.width().max(bounds.height()) / cell).ceil();
    // A count past the cap gives longer pieces, each filed under more cells.
    if pieces.is_nan() || pieces <= 1.0 {
        return vec![*bounds];
    }
    let pieces = pieces.min(4096.0) as usize;
    (0..pieces).map(|k| seg.subsegment(k as f64 / pieces as f64..(k + 1) as f64 / pieces as f64).bounding_box()).collect()
}

/// The inputs' segments (in the sweep's frame), to tell which inputs an edge lies on.
struct Outlines {
    segs: RectIndex<(PathSeg, usize)>,
    tol: f64,
}

impl Outlines {
    fn new(inputs: &[(BezPath, usize)], eps: f64) -> Self {
        // A zero-length segment (a point) is the source of no edge, even one running through it.
        let segs = inputs
            .iter()
            .flat_map(|(bp, i)| bp.segments().map(move |s| (s.bounding_box(), (s, *i))))
            .filter(|(r, ..)| r.width() > 0.0 || r.height() > 0.0)
            .collect();
        // The sweep moves points by a few `eps` at most.
        Self { segs: RectIndex::with_footprint(segs, |r, (s, _), cell| segment_footprint(r, s, cell)), tol: 16.0 * eps }
    }

    /// The front-most input whose outline passes through `p`.
    fn source_at(&self, p: Point) -> Option<usize> {
        use kurbo::ParamCurveNearest;
        let tol = self.tol;
        self.segs.at(p, tol).filter(|(s, _)| s.nearest(p, 1e-9).distance_sq <= tol * tol).map(|(_, i)| *i).max()
    }
}

/// The area an input's fill covers: its closed subpaths (open ones bound no face).
struct Fill {
    path: BezPath,
    bounds: kurbo::Rect,
    rule: FillRule,
}

impl Fill {
    fn new(s: &Shape) -> Self {
        let mut path = BezPath::new();
        for sp in s.path.subpaths.iter().filter(|sp| sp.closed && sp.anchors.len() >= 2) {
            sp.to_bezpath_into(&mut path);
        }
        Self { bounds: path.bounding_box(), path, rule: s.rule }
    }

    fn contains(&self, p: Point) -> bool {
        !self.path.elements().is_empty() && self.bounds.contains(p) && crate::boolean::inside(self.rule, self.path.winding(p))
    }
}

/// One output segment of the sweep: an edge of the planar graph between two vertices.
struct Edge {
    segs: Vec<Seg>,
    ends: [usize; 2],
    /// The input it came from (the front-most, where inputs share it).
    source: usize,
}

/// `segs` (a chain) moved to start at `a` and end at `b`, the handles next to each moving with it.
pub(crate) fn pin_ends(segs: &mut [Seg], a: Point, b: Point) {
    if let Some(s) = segs.first_mut() {
        let d = a - s.c.p0;
        s.c.p0 = a;
        s.c.p1 += d;
    }
    if let Some(s) = segs.last_mut() {
        let d = b - s.c.p3;
        s.c.p3 = b;
        s.c.p2 += d;
    }
}

/// Half-edge `2e` runs along edge `e`, `2e + 1` back along it.
fn twin(h: usize) -> usize {
    h ^ 1
}

/// The segments of half-edge `h`, in its direction.
fn half_segs(edges: &[Edge], h: usize) -> Vec<Seg> {
    half_seg_iter(edges, h).collect()
}

/// The segments of half-edge `h` in its direction, without copying the edge first (an edge can
/// hold thousands of segments, and the angle it leaves a vertex at needs the first few).
fn half_seg_iter(edges: &[Edge], h: usize) -> impl Iterator<Item = Seg> + '_ {
    let segs = edges.get(h / 2).map(|e| e.segs.as_slice()).unwrap_or_default();
    let forward = h.is_multiple_of(2);
    (0..segs.len()).filter_map(move |k| {
        let s = segs.get(if forward { k } else { segs.len() - 1 - k })?;
        Some(if forward { *s } else { Seg { c: CubicBez::new(s.c.p3, s.c.p2, s.c.p1, s.c.p0), line: s.line } })
    })
}

/// Where half-edge `h` ends (the vertex at the other end of its edge).
fn far_end(edges: &[Edge], h: usize) -> Option<Point> {
    let e = edges.get(h / 2)?;
    if h.is_multiple_of(2) { e.segs.last().map(|s| s.c.p3) } else { e.segs.first().map(|s| s.c.p0) }
}

/// The vertex half-edge `h` leaves from.
fn origin(edges: &[Edge], h: usize) -> Option<usize> {
    edges.get(h / 2).map(|e| e.ends[h % 2])
}

/// Where half-edge `h` (leaving `v`) first gets `r` away from it, as an angle around `v`.
///
/// Edges that only meet at `v` cross a circle around it in the order they leave `v` in, however
/// they start out (tangent curves, or the sweep's short straight stubs at a vertex).
fn leaving_angle(edges: &[Edge], h: usize, v: Point, r: f64) -> f64 {
    let first = half_seg_iter(edges, h).next();
    let mut prev = first.map_or(v, |s| s.c.p0);
    let mut hit: Option<Point> = None;
    // One segment at a time, stopping at the first that gets r away: an edge can run on for
    // thousands of segments (a traced outline between two crossings), all of which flattening
    // the whole edge to this tolerance would visit, at every vertex.
    for s in half_seg_iter(edges, h) {
        let path = [kurbo::PathEl::MoveTo(s.c.p0), kurbo::PathEl::CurveTo(s.c.p1, s.c.p2, s.c.p3)];
        kurbo::flatten(path, (r * 1e-4).max(1e-12), |el| {
            if let (None, kurbo::PathEl::LineTo(p)) = (hit, el) {
                if p.distance(v) >= r {
                    // The point on prev → p at distance r (prev is closer).
                    let (d, f) = (p - prev, prev - v);
                    let (a, b, c) = (d.hypot2(), 2.0 * f.dot(d), f.hypot2() - r * r);
                    let t = if a > 0.0 { ((-b + (b * b - 4.0 * a * c).max(0.0).sqrt()) / (2.0 * a)).clamp(0.0, 1.0) } else { 1.0 };
                    hit = Some(prev + d * t);
                }
                prev = p;
            }
        });
        if hit.is_some() {
            break;
        }
    }
    // An edge too short to reach r (it can't, r is at most half of every chord) leaves along its
    // tangent.
    let far = hit.unwrap_or_else(|| first.map_or(v, |s| [s.c.p1, s.c.p2, s.c.p3].into_iter().find(|p| p.distance(s.c.p0) > 0.0).unwrap_or(s.c.p3)));
    (far - v).atan2()
}

/// Path of a closed run of half-edges, tidied (split pieces of one input curve joined again).
fn cycle_bezpath(edges: &[Edge], cycle: &[usize]) -> BezPath {
    let mut bp = BezPath::new();
    for (k, s) in cycle.iter().flat_map(|&h| half_segs(edges, h)).enumerate() {
        if k == 0 {
            bp.move_to(s.c.p0);
        }
        if s.line {
            bp.line_to(s.c.p3);
        } else {
            bp.curve_to(s.c.p1, s.c.p2, s.c.p3);
        }
    }
    bp.close_path();
    bp
}

/// `cycle` without the spurs it runs out and back along (edges with the same face on both sides,
/// such as a line's loose end).
fn without_spurs(cycle: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(cycle.len());
    for &h in cycle {
        if out.last() == Some(&twin(h)) {
            out.pop();
        } else {
            out.push(h);
        }
    }
    // A spur where the walk started wraps around the ends.
    while out.len() >= 2 && out.first().map(|&h| twin(h)) == out.last().copied() {
        out.pop();
        out.remove(0);
    }
    out
}

/// Append `open` traced there and back (a closed, zero-area path along it) to `out`.
fn there_and_back(open: &BezPath, out: &mut BezPath) {
    let segs: Vec<PathSeg> = open.segments().collect();
    let Some(first) = segs.first() else { return };
    out.move_to(first.start());
    for s in &segs {
        out.push(s.as_path_el());
    }
    for s in segs.iter().rev() {
        out.push(s.reverse().as_path_el());
    }
    out.close_path();
}

/// `path` back from the sweep's frame, its anchors within `tol` of an input anchor (`sorted` by x)
/// put exactly on it, handles moving with them.
fn unshear(mut path: PathData, sorted: &[Point], tol: f64) -> PathData {
    path.transform(UNSHEAR);
    for a in path.subpaths.iter_mut().flat_map(|sp| sp.anchors.iter_mut()) {
        let lo = sorted.partition_point(|q| q.x < a.p.x - tol);
        if let Some(q) = sorted.get(lo..).unwrap_or_default().iter().take_while(|q| q.x <= a.p.x + tol).find(|q| (q.y - a.p.y).abs() <= tol) {
            let d = *q - a.p;
            a.p = *q;
            a.h_in += d;
            a.h_out += d;
        }
    }
    path
}

fn find(parent: &mut [usize], mut v: usize) -> usize {
    while let Some(&p) = parent.get(v) {
        if p == v {
            break;
        }
        let gp = parent.get(p).copied().unwrap_or(p);
        if let Some(slot) = parent.get_mut(v) {
            *slot = gp;
        }
        v = p;
    }
    v
}

/// The Live Paint planar map of `shapes` (back → front).
///
/// Faces are the bounded areas the paths enclose, holes included, each with the inputs whose fill
/// covers it (ascending; empty for an area only enclosed by paths). Edges are the paths split
/// wherever they meet another path, keyed by the input they came from (the front-most where
/// inputs share one). Empty when the paths can't be swept (non-finite or too degenerate).
pub fn live_paint(shapes: &[Shape]) -> (Vec<Region>, Vec<Shape>) {
    build(shapes, DEFAULT_PRECISION).unwrap_or_default()
}

/// [`live_paint`] with Gap Detection: the faces are those of `shapes` with a line across each gap
/// [`find_gaps`] finds up to `max_gap`, so paint stops at the gaps, and the edges are those of
/// `shapes` alone (the lines add none). Also returns the gaps.
pub fn live_paint_with_gaps(shapes: &[Shape], max_gap: f64) -> (Vec<Region>, Vec<Shape>, Vec<[Point; 2]>) {
    let gaps = find_gaps(shapes, max_gap);
    if gaps.is_empty() {
        let (faces, edges) = live_paint(shapes);
        return (faces, edges, gaps);
    }
    let mut closed = shapes.to_vec();
    let lines = PathData::new(gaps.iter().map(|g| SubPath::polyline(g, false)).collect());
    closed.push(Shape::new(lines, FillRule::NonZero, u64::MAX));
    let (faces, _) = live_paint(&closed);
    let (_, edges) = live_paint(shapes);
    (faces, edges, gaps)
}

/// How many times longer than a gap the way between its two ends along the paths must be for it to
/// count: a loose end that crosses a path just before it (an overshoot), or a path that curls back
/// close to its own end (a hook), leaves no gap there.
const GAP_DETOUR: f64 = 8.0;

/// How exactly [`find_gaps`] measures lengths along the paths.
const GAP_ARC_ACCURACY: f64 = 1e-4;

/// How many points along a loose end's own segment [`find_gaps`] tries.
const GAP_SAMPLES: u32 = 64;

/// One subpath's segments for [`find_gaps`], with the length along it where each starts.
struct Track {
    segs: Vec<PathSeg>,
    /// The length at the start of each segment, then the whole length.
    at: Vec<f64>,
    closed: bool,
    bounds: kurbo::Rect,
}

impl Track {
    fn new(sp: &SubPath) -> Option<Self> {
        if sp.anchors.len() < 2 || !sp.anchors.iter().all(|a| a.p.is_finite() && a.h_in.is_finite() && a.h_out.is_finite()) {
            return None;
        }
        let mut bp = BezPath::new();
        sp.to_bezpath_into(&mut bp);
        let segs: Vec<PathSeg> = bp.segments().collect();
        let mut at = Vec::with_capacity(segs.len() + 1);
        let mut len = 0.0;
        for s in &segs {
            at.push(len);
            len += s.arclen(GAP_ARC_ACCURACY);
        }
        at.push(len);
        (!segs.is_empty()).then(|| Self { bounds: bp.bounding_box(), segs, at, closed: sp.closed })
    }

    fn len(&self) -> f64 {
        self.at.last().copied().unwrap_or(0.0)
    }

    /// The length along the subpath to parameter `t` of segment `k`.
    fn pos(&self, k: usize, t: f64) -> f64 {
        let Some(s) = self.segs.get(k) else { return self.len() };
        self.at.get(k).copied().unwrap_or(0.0) + s.subsegment(0.0..t).arclen(GAP_ARC_ACCURACY)
    }

    /// The length along the subpath between two places on it (the shorter way round a closed one).
    fn along(&self, a: f64, b: f64) -> f64 {
        let d = (a - b).abs();
        if self.closed { d.min(self.len() - d) } else { d }
    }
}

/// Do two boxes (lines' boxes included, which have no area) touch or overlap?
fn boxes_meet(a: kurbo::Rect, b: kurbo::Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Where track `o`, within `reach` along it of `from`, crosses the segments `near` of track `q`:
/// (length along `o`, length along `q`) of each crossing.
fn crossings(o: &Track, from: f64, reach: f64, q: &Track, near: impl Iterator<Item = usize>) -> Vec<(f64, f64)> {
    let near: Vec<(usize, &PathSeg)> = near.filter_map(|j| Some((j, q.segs.get(j)?))).collect();
    let mut out = vec![];
    // `first` is the first segment that ends within reach.
    let first = o.at.partition_point(|&a| a < from - reach).saturating_sub(1);
    for (k, s) in o.segs.iter().enumerate().skip(first) {
        let (Some(&a0), Some(&a1)) = (o.at.get(k), o.at.get(k + 1)) else { continue };
        if a0 > from + reach {
            break;
        }
        if a1 < from - reach || !boxes_meet(s.bounding_box(), q.bounds) {
            continue;
        }
        // A curve is tried as 16 chords.
        let n = if matches!(s, PathSeg::Line(_)) { 1 } else { 16 };
        let mut prev = s.start();
        for i in 1..=n {
            let (t0, t1) = (f64::from(i - 1) / f64::from(n), f64::from(i) / f64::from(n));
            let p = s.eval(t1);
            let chord = kurbo::Line::new(prev, p);
            let cb = chord.bounding_box();
            for &(j, qs) in &near {
                if !boxes_meet(qs.bounding_box(), cb) {
                    continue;
                }
                for x in qs.intersect_line(chord) {
                    out.push((o.pos(k, t0 + x.line_t * (t1 - t0)), q.pos(j, x.segment_t)));
                }
            }
            prev = p;
        }
    }
    out
}

/// Most grid cells one segment is listed in by [`SegGrid`]; a longer one is tried for every point.
const GAP_GRID_SPAN: i64 = 1024;

/// The farthest cell from the grid's origin, on each axis, that [`SegGrid`] numbers a point in.
const GAP_GRID_LIMIT: f64 = 1_048_576.0;

/// The segments of [`find_gaps`]' tracks by the cells of a square grid that their bounds cover.
struct SegGrid {
    origin: Point,
    cell: f64,
    /// (track, segment) by cell.
    cells: HashMap<(i64, i64), Vec<(usize, usize)>>,
    /// Segments whose bounds cover more than [`GAP_GRID_SPAN`] cells, near every point.
    wide: Vec<(usize, usize)>,
}

impl SegGrid {
    fn new(tracks: &[Track], origin: Point, cell: f64) -> Self {
        let mut grid = Self { origin, cell, cells: HashMap::new(), wide: vec![] };
        for (j, t) in tracks.iter().enumerate() {
            for (k, s) in t.segs.iter().enumerate() {
                let b = s.bounding_box();
                let ((x0, y0), (x1, y1)) = (grid.cell_of(Point::new(b.x0, b.y0)), grid.cell_of(Point::new(b.x1, b.y1)));
                if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > GAP_GRID_SPAN {
                    grid.wide.push((j, k));
                    continue;
                }
                for x in x0..=x1 {
                    for y in y0..=y1 {
                        grid.cells.entry((x, y)).or_default().push((j, k));
                    }
                }
            }
        }
        grid
    }

    /// The cell `p` is in, counted from the origin (at most [`GAP_GRID_LIMIT`] cells away on each
    /// axis).
    fn cell_of(&self, p: Point) -> (i64, i64) {
        let at = |v: f64, o: f64| ((v - o) / self.cell).floor().clamp(-GAP_GRID_LIMIT, GAP_GRID_LIMIT) as i64;
        (at(p.x, self.origin.x), at(p.y, self.origin.y))
    }

    /// The segments whose bounds come within `r` of `p` (and some farther ones), each once, in
    /// track order.
    fn near(&self, p: Point, r: f64) -> Vec<(usize, usize)> {
        let mut out = self.wide.clone();
        let ((x0, y0), (x1, y1)) = (self.cell_of(Point::new(p.x - r, p.y - r)), self.cell_of(Point::new(p.x + r, p.y + r)));
        for x in x0..=x1 {
            for y in y0..=y1 {
                out.extend(self.cells.get(&(x, y)).into_iter().flatten());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The point a gap from the loose end `e` of track `own` (at `from` along it) closes to: the
/// nearest point of any path within `max_gap` of it, leaving out points the paths already join to
/// `e` along a way less than [`GAP_DETOUR`] times as long as the gap. None when there is no such
/// point, or when `e` lies on a path (it is no loose end).
fn gap_from(tracks: &[Track], grid: &SegGrid, own: usize, e: Point, from: f64, max_gap: f64, eps: f64) -> Option<Point> {
    use kurbo::ParamCurveNearest;
    let o = tracks.get(own)?;
    let reach = GAP_DETOUR * max_gap;
    let mut best: Option<(f64, Point)> = None;
    // `around` holds the segments a way of up to `reach` from `e` can cross, by track (grouped
    // once: filtering the whole list again for each track near `e` made clusters of parallel
    // paths cubic), and `crossed` the crossings with each track.
    let mut around: Option<HashMap<usize, Vec<usize>>> = None;
    let mut crossed: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    for (j, k) in grid.near(e, max_gap) {
        let Some(q) = tracks.get(j) else { continue };
        let Some(s) = q.segs.get(k) else { continue };
        let b = s.bounding_box();
        if e.x < b.x0 - max_gap || e.x > b.x1 + max_gap || e.y < b.y0 - max_gap || e.y > b.y1 + max_gap {
            continue;
        }
        let near = s.nearest(e, 1e-9);
        let d = near.distance_sq.sqrt();
        if d > max_gap {
            continue;
        }
        let at = q.pos(k, near.t);
        let way = if j == own {
            o.along(from, at)
        } else {
            let xs = crossed.entry(j).or_insert_with(|| {
                let by_track = around.get_or_insert_with(|| {
                    let mut by_track: HashMap<usize, Vec<usize>> = HashMap::new();
                    for (t, k) in grid.near(e, reach + max_gap) {
                        by_track.entry(t).or_default().push(k);
                    }
                    by_track
                });
                crossings(o, from, reach, q, by_track.get(&j).into_iter().flatten().copied())
            });
            xs.iter().map(|&(a, b)| o.along(from, a) + q.along(b, at)).fold(f64::INFINITY, f64::min)
        };
        if d <= eps {
            // On another path, or on its own away from the end: the end is joined there.
            if j != own || way > 4.0 * eps.max(GAP_ARC_ACCURACY) {
                return None;
            }
            // The end's own segment: a curve that comes back round can still pass near it.
            for i in 0..=GAP_SAMPLES {
                let t = f64::from(i) / f64::from(GAP_SAMPLES);
                let p = s.eval(t);
                let d = p.distance(e);
                if d > eps && d <= max_gap && o.along(from, q.pos(k, t)) > GAP_DETOUR * d && best.is_none_or(|b| d < b.0) {
                    best = Some((d, p));
                }
            }
            continue;
        }
        if way > GAP_DETOUR * d && best.is_none_or(|b| d < b.0) {
            best = Some((d, s.eval(near.t)));
        }
    }
    best.map(|b| b.1)
}

/// The gaps Live Paint's Gap Detection closes in `shapes`: from each loose end of an open subpath
/// (an end that lies on no path) to the nearest point of any path, its own included, at most
/// `max_gap` away, unless the paths already join the two along a way less than [`GAP_DETOUR`] times
/// the gap's length. Each gap is a pair of points: the loose end, then where it closes to. A gap
/// between two loose ends that close to each other is listed once.
pub fn find_gaps(shapes: &[Shape], max_gap: f64) -> Vec<[Point; 2]> {
    if !(max_gap.is_finite() && max_gap > 0.0) {
        return vec![];
    }
    let tracks: Vec<Track> = shapes.iter().flat_map(|s| &s.path.subpaths).filter_map(Track::new).collect();
    let eps = (max_gap * 1e-4).max(1e-6);
    // The grid cells are as wide as the gap, or 1/4096 of the art where that is wider.
    let bounds = tracks.iter().map(|t| t.bounds).reduce(|a, b| a.union(b)).unwrap_or_default();
    let grid = SegGrid::new(&tracks, bounds.origin(), max_gap.max(bounds.width().max(bounds.height()) / 4096.0));
    let mut gaps: Vec<[Point; 2]> = vec![];
    // `by_end` lists the gaps by the cell of their loose end.
    let mut by_end: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, o) in tracks.iter().enumerate() {
        let (Some(first), Some(last)) = (o.segs.first(), o.segs.last()) else { continue };
        // An open subpath that ends where it starts encloses its area already.
        if o.closed || first.start().distance(last.end()) <= eps {
            continue;
        }
        for (e, from) in [(first.start(), 0.0), (last.end(), o.len())] {
            let Some(to) = gap_from(&tracks, &grid, i, e, from, max_gap, eps) else { continue };
            let (cx, cy) = grid.cell_of(to);
            let listed = (cx - 1..=cx + 1)
                .flat_map(|x| (cy - 1..=cy + 1).map(move |y| (x, y)))
                .filter_map(|c| by_end.get(&c))
                .flatten()
                .filter_map(|&g| gaps.get(g))
                .any(|g| g[0].distance(to) <= eps && g[1].distance(e) <= eps);
            if !listed {
                by_end.entry(grid.cell_of(e)).or_default().push(gaps.len());
                gaps.push([e, to]);
            }
        }
    }
    gaps
}

/// Most segments the Shape Builder sweeps into its planar map (open paths as edges, the gaps closed
/// paths enclose, outline edges); past this only the filled areas count. The map costs about what
/// the filled areas' arrangement does (both near linear in the input; the cap was 4096 while
/// finding each edge's source scanned every segment, #937), so this only bounds the time a hover
/// can take on art far past what a drawing holds.
pub const SHAPE_BUILDER_MAX_SEGMENTS: usize = 65_536;

/// The Shape Builder's arrangement: see [`shape_builder`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuilderArrangement {
    pub regions: Vec<Region>,
    /// The pieces of the paths with no closed subpath, keyed by input index.
    pub lines: Vec<Shape>,
    /// The pieces of the other paths' outlines, keyed by input index (the front-most where inputs
    /// share one); only when asked for.
    pub edges: Vec<Shape>,
}

/// Does any of `shapes` have an open subpath (a cutting edge for the Shape Builder)?
pub fn has_open_path(shapes: &[Shape]) -> bool {
    shapes.iter().flat_map(|s| &s.path.subpaths).any(|sp| !sp.closed && sp.anchors.len() >= 2)
}

/// Shape Builder regions of `shapes` (back → front), the pieces of their open paths and, with
/// `edges`, the pieces of their outlines (what erasing an edge deletes).
///
/// Closed paths bound regions with their filled areas, as in [`regions`](crate::regions), and the
/// gaps they enclose that nothing fills are regions too (empty `sources`), such as the middle of
/// four circles that touch (#1691). Open paths (the caller closes the ones whose fill should count)
/// are cutting edges: a line across a shape splits it, lines around an area enclose a region
/// nothing fills, and each path with no closed subpath comes back cut wherever another path meets
/// it, as pieces keyed by its index in `shapes`. Outlines are cut the same way. With more than
/// [`SHAPE_BUILDER_MAX_SEGMENTS`] segments or a map that can't be built, only the filled areas
/// count and there are no pieces.
pub fn shape_builder(shapes: &[Shape], edges: bool) -> BuilderArrangement {
    shape_builder_with_gap(shapes, edges, 0.0)
}

/// Most distance (document units) a Shape Builder gap may span.
pub const SHAPE_BUILDER_MAX_GAP: f64 = 72.0;

/// Original inputs and the shared detector's temporary, unfilled bridges. Keep the originals'
/// order and keys so only their split pieces can be returned as art.
pub(crate) fn gap_inputs(shapes: &[Shape], gap: f64) -> Vec<Shape> {
    let mut closed = shapes.to_vec();
    let segments: usize = shapes.iter().flat_map(|s| &s.path.subpaths).map(|sp| sp.segment_count()).sum();
    if segments <= SHAPE_BUILDER_MAX_SEGMENTS && gap.is_finite() && gap > 0.0 && has_open_path(shapes) {
        let gaps = find_gaps(shapes, gap.min(SHAPE_BUILDER_MAX_GAP));
        if !gaps.is_empty() {
            let path = PathData::new(gaps.iter().map(|g| SubPath::polyline(g, false)).collect());
            closed.push(Shape::new(path, FillRule::NonZero, u64::MAX));
        }
    }
    closed
}

/// [`shape_builder`] with Gap Detection: open paths whose ends come within `gap` (document units)
/// of another path are bridged for the regions they bound, so a region left open by a small gap is
/// found. A `gap` of 0 is [`shape_builder`]. The bridges are in no region's sources and in no piece
/// of `lines` or `edges`.
pub fn shape_builder_with_gap(shapes: &[Shape], edges: bool, gap: f64) -> BuilderArrangement {
    builder(shapes, edges, gap, DEFAULT_PRECISION)
}

/// [`shape_builder_with_gap`] refitting its results to `precision` (Pathfinder Options › Precision).
pub(crate) fn builder(shapes: &[Shape], edges: bool, gap: f64, precision: f64) -> BuilderArrangement {
    let has_open = has_open_path(shapes);
    let segments: usize = shapes.iter().flat_map(|s| &s.path.subpaths).map(|sp| sp.segment_count()).sum();
    let planar = (segments <= SHAPE_BUILDER_MAX_SEGMENTS)
        .then(|| {
            let keyed: Vec<Shape> = shapes.iter().enumerate().map(|(i, s)| Shape::new(s.path.clone(), s.rule, i as u64)).collect();
            if has_open && gap.is_finite() && gap > 0.0 {
                let (faces, pieces) = build(&gap_inputs(&keyed, gap), precision).unwrap_or_default();
                // Gap targets split original outlines too, but the bridges themselves aren't art.
                (faces, pieces.into_iter().filter(|p| p.key < shapes.len() as u64).collect())
            } else {
                build(&keyed, precision).unwrap_or_default()
            }
        })
        .filter(|(faces, pieces)| !faces.is_empty() || !pieces.is_empty());
    let Some((faces, pieces)) = planar else {
        return BuilderArrangement { regions: filled_regions(shapes, precision), ..Default::default() };
    };
    let is_line = |i: u64| shapes.get(i as usize).is_some_and(|s| !s.path.subpaths.iter().any(|sp| sp.closed));
    let (lines, outline): (Vec<Shape>, Vec<Shape>) = pieces.into_iter().partition(|e| is_line(e.key));
    let regions = if has_open {
        faces
    } else {
        // The filled areas from their arrangement, whose faces merge exactly, then the gaps.
        let mut regions = crate::pathfinder::regions_with(shapes, precision);
        regions.extend(faces.into_iter().filter(|f| f.sources.is_empty()));
        regions
    };
    BuilderArrangement { regions, lines: if has_open { lines } else { Vec::new() }, edges: if edges { outline } else { Vec::new() } }
}

/// The regions of the shapes' filled areas. A shape that fills nothing (open paths that enclose no
/// area, such as cutting lines) is left out, so a flood of them cannot push the arrangement past its
/// size budget and take the filled shapes down with it; the regions keep the original indices.
fn filled_regions(shapes: &[Shape], precision: f64) -> Vec<crate::Region> {
    let fills = |s: &Shape| s.path.subpaths.iter().any(|sp| sp.closed || encloses_area(sp));
    let keep: Vec<usize> = (0..shapes.len()).filter(|&i| shapes.get(i).is_some_and(fills)).collect();
    if keep.len() == shapes.len() {
        return crate::pathfinder::regions_with(shapes, precision);
    }
    let kept: Vec<Shape> = keep.iter().filter_map(|&i| shapes.get(i).cloned()).collect();
    crate::pathfinder::regions_with(&kept, precision)
        .into_iter()
        .map(|mut r| {
            r.sources = r.sources.iter().filter_map(|&s| keep.get(s).copied()).collect();
            r
        })
        .collect()
}

/// Does `sp`, closed, enclose an area (rather than retrace itself)? The Shape Builder closes the
/// open subpaths of filled paths that do.
pub fn encloses_area(sp: &SubPath) -> bool {
    let mut closed = sp.clone();
    closed.closed = true;
    let mut bp = BezPath::new();
    closed.to_bezpath_into(&mut bp);
    let size = bp.bounding_box().size();
    bp.area().abs() > 1e-9 * (size.width * size.width + size.height * size.height)
}

/// `path` without `piece`, a run along one of its subpaths between two points (a Shape Builder
/// edge): a closed subpath opens there, an open one splits in two (or loses an end), and a piece
/// that runs all the way round takes its subpath. With `fill_closes`, open subpaths that enclose an
/// area ([`encloses_area`]) run on round an invisible closing edge, as the Shape Builder sees a
/// filled path: a piece may run over it, and it never comes back as a visible segment. None when
/// the piece doesn't lie on `path` within `tol`.
pub fn cut_out(path: &PathData, piece: &SubPath, fill_closes: bool, tol: f64) -> Option<PathData> {
    let n = piece.segment_count();
    if n == 0 {
        return None;
    }
    // A point well inside the piece: the middle of its longest segment.
    let arclen = |k: usize| kurbo::ParamCurveArclen::arclen(&piece.segment(k), 1e-3);
    let mid = (0..n).map(|k| (arclen(k), k)).max_by(|x, y| x.0.total_cmp(&y.0)).map(|(_, k)| piece.segment(k).eval(0.5))?;
    let (a, b) = (piece.anchors.first()?.p, piece.anchors.last()?.p);
    let mut bp = BezPath::new();
    piece.to_bezpath_into(&mut bp);
    if bp.perimeter(1e-6) <= tol {
        // A speck (where paths touch): there's nothing to cut.
        return None;
    }
    // A piece that ends where it starts runs all the way round.
    let whole = piece.closed || a.distance(b) <= tol;
    // Each subpath as the piece may run along it: round its closing edge when it has one.
    let ring = |sp: &SubPath| SubPath { anchors: sp.anchors.clone(), closed: sp.closed || (fill_closes && encloses_area(sp)) };
    let (si, geom, um) = path
        .subpaths
        .iter()
        .enumerate()
        .filter_map(|(i, sp)| {
            let g = ring(sp);
            let (u, d) = nearest_u(&g, mid)?;
            (d <= tol).then_some((i, g, u, d))
        })
        .min_by(|x, y| x.3.total_cmp(&y.3))
        .map(|(i, g, u, _)| (i, g, u))?;
    let len = geom.segment_count() as f64;
    let real = path.subpaths.get(si)?.segment_count() as f64;
    let at = |p: Point| nearest_u(&geom, p).filter(|(_, d)| *d <= tol).map(|(u, _)| u);
    // The parameter spans of `geom` that stay (unrolled past the end of a ring).
    let keep: Vec<(f64, f64)> = if whole {
        Vec::new()
    } else if geom.closed {
        let (ua, ub) = (at(a)?, at(b)?);
        let fwd = |from: f64, to: f64| (to - from).rem_euclid(len);
        // The piece runs from one end through `mid` to the other; what stays runs on round.
        let (start, end) = if fwd(ua, um) <= fwd(ua, ub) { (ub, ua) } else { (ua, ub) };
        let rest = fwd(start, end);
        if rest <= 1e-9 || len - rest <= 1e-9 {
            Vec::new()
        } else {
            // Where the ring closes with an invisible edge (`real..len`), what stays stops at it.
            let parts: &[(f64, f64)] = if real >= len { &[(0.0, 2.0 * len)] } else { &[(0.0, real), (len, len + real)] };
            parts.iter().map(|&(lo, hi)| (start.max(lo), (start + rest).min(hi))).filter(|(x, y)| y - x > 1e-9).collect()
        }
    } else {
        let (ua, ub) = (at(a)?, at(b)?);
        let (lo, hi) = (ua.min(ub), ua.max(ub));
        if !(lo - 1e-9..=hi + 1e-9).contains(&um) {
            return None;
        }
        [(0.0, lo), (hi, len)].into_iter().filter(|(x, y)| y - x > 1e-9).collect()
    };
    let mut subpaths: Vec<SubPath> = path.subpaths.iter().take(si).cloned().collect();
    subpaths.extend(keep.into_iter().filter_map(|(u0, u1)| span(&geom, u0, u1)).map(|sp| snap_ends(sp, [a, b], tol)));
    subpaths.extend(path.subpaths.iter().skip(si + 1).cloned());
    Some(PathData::new(subpaths))
}

/// The run of `piece` (a Shape Builder edge or line piece) around the point of it nearest `at`,
/// from the nearest of `stops` before that point to the nearest after it, open. Stops are the
/// points within `tol` of the piece, such as the anchors of the path it lies on; an open piece's
/// own ends bound the run where no stop comes first. None when the piece has no segments, or when
/// it is closed and fewer than two different stops lie on it (the run would be all of it).
pub fn run_around(piece: &SubPath, stops: &[Point], at: Point, tol: f64) -> Option<SubPath> {
    let n = piece.segment_count();
    if n == 0 {
        return None;
    }
    let len = n as f64;
    let (u, _) = nearest_u(piece, at)?;
    // The stops by x, so that each segment measures only the stops within its bounds.
    let mut by_x: Vec<Point> = stops.iter().copied().filter(|s| s.x.is_finite() && s.y.is_finite()).collect();
    by_x.sort_by(|a, b| a.x.total_cmp(&b.x));
    // The parameters of the stops within `tol` of segment `j` whose nearest point on the whole
    // piece lies on it (a ring's segments repeat past either end, `len` apart).
    let on = |j: isize| -> Vec<f64> {
        use kurbo::ParamCurveNearest;
        let seg = piece.segment(j.rem_euclid(n as isize) as usize);
        let b = kurbo::Shape::bounding_box(&seg).inflate(tol, tol);
        let near = by_x.get(by_x.partition_point(|s| s.x < b.x0)..).unwrap_or_default();
        near.iter()
            .take_while(|s| s.x <= b.x1)
            .filter(|s| s.y >= b.y0 && s.y <= b.y1)
            .filter_map(|s| {
                let m = seg.nearest(*s, 1e-9);
                let d = m.distance_sq.sqrt();
                (d <= tol && nearest_u(piece, *s).is_some_and(|(_, nearest)| d <= nearest + 1e-9)).then_some(j as f64 + m.t)
            })
            .collect()
    };
    // From the segment under `at`, the walk goes back to the nearest stop at or before it and on
    // to the nearest stop after it; an open piece's ends bound it.
    let k0 = (u.floor().max(0.0) as isize).min(n as isize - 1);
    let (first, last) = if piece.closed { (k0 - n as isize, k0 + n as isize) } else { (0, n as isize - 1) };
    let lo = (first..=k0).rev().find_map(|j| on(j).into_iter().filter(|&x| x <= u).max_by(f64::total_cmp));
    let lo = match lo {
        Some(lo) => lo,
        None if !piece.closed => 0.0,
        None => return None,
    };
    let after = u.max(lo + 1e-9);
    let hi = (k0..=last).find_map(|j| on(j).into_iter().filter(|&x| x > after).min_by(f64::total_cmp));
    let hi = match hi {
        Some(hi) => hi,
        None if !piece.closed => len,
        None => return None,
    };
    // On a ring, a lone stop would make the run all of it.
    if piece.closed && hi - lo >= len - 1e-9 {
        return None;
    }
    // A run that passes the ring's start begins before its end and is unrolled past it.
    let (lo, hi) = if lo < 0.0 { (lo + len, hi + len) } else { (lo, hi) };
    let run = span(piece, lo, hi)?;
    // The run's ends go exactly on the stops they end at.
    let stop_at =
        |q: Point| stops.iter().copied().filter(|s| s.distance(q) <= tol).min_by(|a, b| a.distance(q).total_cmp(&b.distance(q))).unwrap_or(q);
    let ends = [run.anchors.first()?.p, run.anchors.last()?.p].map(stop_at);
    Some(snap_ends(run, ends, tol))
}

/// The point of `sp` nearest `p`, as a parameter `segment + t`, with its distance.
fn nearest_u(sp: &SubPath, p: Point) -> Option<(f64, f64)> {
    use kurbo::ParamCurveNearest;
    (0..sp.segment_count())
        .map(|k| {
            let n = sp.segment(k).nearest(p, 1e-9);
            (k as f64 + n.t, n.distance_sq.sqrt())
        })
        .min_by(|x, y| x.1.total_cmp(&y.1))
}

/// `sp` with its end anchors within `tol` of one of `to` put exactly on it (where the planar map
/// put the junction a piece ends at), handles moving with them.
fn snap_ends(mut sp: SubPath, to: [Point; 2], tol: f64) -> SubPath {
    let last = sp.anchors.len().saturating_sub(1);
    for i in [0, last] {
        if let Some(an) = sp.anchors.get_mut(i)
            && let Some(q) = to.iter().find(|q| q.distance(an.p) <= tol)
        {
            let d = *q - an.p;
            an.p = *q;
            an.h_in += d;
            an.h_out += d;
        }
    }
    sp
}

/// The run of `sp` from parameter `u0` to `u1` (`u0 < u1`, unrolled past the end of a closed one),
/// open.
fn span(sp: &SubPath, u0: f64, u1: f64) -> Option<SubPath> {
    let n = sp.segment_count();
    if n == 0 || !(u0.is_finite() && u1.is_finite()) {
        return None;
    }
    let first = u0.floor().max(0.0) as usize;
    let last = (u1.ceil().max(0.0) as usize).min(first + 2 * n);
    let segs: Vec<Seg> = (first..last)
        .filter_map(|k| {
            let (t0, t1) = ((u0 - k as f64).clamp(0.0, 1.0), (u1 - k as f64).clamp(0.0, 1.0));
            (t1 - t0 > 1e-12).then(|| Seg { c: sp.segment(k % n).subsegment(t0..t1), line: sp.segment_is_line(k % n) })
        })
        .collect();
    segs_to_subpath(&segs, false)
}

/// What is left of input `shape` when the faces that hold `merged` merge on their own, with the
/// inputs that cover them marked in `cover` ([`Map::merge`]).
pub(crate) struct Rest {
    pub(crate) shape: usize,
    pub(crate) merged: Vec<Point>,
    pub(crate) cover: Vec<bool>,
}

/// The planar graph of `shapes` (in the sweep's frame) with its faces walked: what [`build`] makes
/// Live Paint faces and edges from, and what [`Map::merge`] merges faces in.
pub(crate) struct Map {
    edges: Vec<Edge>,
    around: Vec<Vec<usize>>,
    slot_of: Vec<usize>,
    /// Bounded faces: their cycle of half-edges, path and area; and, by their bounds, which can
    /// hold a point, so finding the face around one tests only those.
    faces: Vec<(Vec<usize>, BezPath, f64)>,
    face_index: RectIndex<usize>,
    /// Bounded faces left out as slivers ([`is_sliver`]): their cycle of half-edges and the inputs
    /// whose fill covers them (none for a gap, such as a thin cutout).
    slivers: Vec<(Vec<usize>, Vec<usize>)>,
    /// Outer boundaries of connected pieces, and the face each one is a hole in.
    holes: Vec<(Vec<usize>, BezPath)>,
    face_holes: Vec<Vec<usize>>,
    tidy: Tidy,
    /// The inputs' anchors (sorted by x) and how close results snap back onto them.
    anchors: Vec<Point>,
    snap: f64,
}

/// With `cover`, also finds which inputs cover each sliver, which only [`Map::merge`] reads;
/// without it the slivers' lists of inputs stay empty.
pub(crate) fn map(shapes: &[Shape], cover: bool) -> Option<Map> {
    map_with(shapes, cover, DEFAULT_PRECISION)
}

/// [`map`] whose faces and pieces are refit to `precision`, and whose slivers thinner than it are dropped.
fn map_with(shapes: &[Shape], cover: bool, precision: f64) -> Option<Map> {
    let mut inputs: Vec<(BezPath, usize)> = Vec::new();
    for (i, s) in shapes.iter().enumerate() {
        let mut bp = BezPath::new();
        for sp in s.path.subpaths.iter().filter(|sp| sp.anchors.len() >= 2) {
            if sp.closed {
                sp.to_bezpath_into(&mut bp);
            } else {
                let mut open = BezPath::new();
                sp.to_bezpath_into(&mut open);
                there_and_back(&open, &mut bp);
            }
        }
        if !bp.elements().is_empty() {
            bp.apply_affine(SHEAR);
            inputs.push((bp, i));
        }
    }
    if inputs.is_empty() {
        return None;
    }
    let eps = eps_for(inputs.iter().map(|p| &p.0)).ok()?;
    snap_horizontals(&mut inputs.iter_mut().map(|p| &mut p.0).collect::<Vec<_>>(), eps);
    // Counted on the snapped geometry, which is what the sweep sees: snapping can bring boxes into contact.
    if !crate::budget::sweep_fits(inputs.iter().map(|p| &p.0)) {
        return None;
    }
    // The sweep can panic on rare near-degenerate input; that must never take the app down.
    let sweep =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Topology::<Crossings>::from_paths(inputs.iter().map(|(p, i)| (p, *i)), eps)));
    let top = sweep.ok()?.ok()?;
    let tidy = Tidy::keeping(precision, inputs.iter().map(|p| &p.0));

    // The planar graph: vertices are the sweep's points, edges its segments. Points closer than
    // the sweep's tolerance are one vertex (it can put a corner that lies on another path's edge
    // a rounding error away from where it cuts that edge).
    let positions = top.compute_positions();
    let cell = 4.0 * eps;
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut points: Vec<Point> = Vec::new();
    let mut vertex_id = |p: Point| {
        let (cx, cy) = ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
        let near = (cx - 1..=cx + 1)
            .flat_map(|x| (cy - 1..=cy + 1).map(move |y| (x, y)))
            .filter_map(|k| grid.get(&k))
            .flatten()
            .copied()
            .find(|&v| points.get(v).is_some_and(|q| q.distance(p) <= eps));
        near.unwrap_or_else(|| {
            points.push(p);
            grid.entry((cx, cy)).or_default().push(points.len() - 1);
            points.len() - 1
        })
    };
    let mut pieces: Vec<(Vec<Seg>, [usize; 2])> = Vec::new();
    for idx in top.segment_indices() {
        let segs: Vec<Seg> = positions[idx].path.segments().map(to_seg).collect();
        let ends = [vertex_id(top.point(idx.first_half()).to_kurbo()), vertex_id(top.point(idx.second_half()).to_kurbo())];
        if !segs.is_empty() && ends[0] != ends[1] {
            pieces.push((segs, ends));
        }
    }
    // The sweep can cut the paths through one crossing at points more than `eps` apart, so copies of
    // one edge (such as an open path's two passes) can end on different vertices. Pieces whose
    // middles and ends lie within `tol` of each other are such copies, and their ends are one vertex.
    let tol = 16.0 * eps;
    let cell_of = |p: Point| ((p.x / tol).floor() as i64, (p.y / tol).floor() as i64);
    let mids: Vec<Option<Point>> = pieces.iter().map(|(segs, _)| mid(segs)).collect();
    let mut by_mid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, m) in mids.iter().enumerate() {
        if let Some(m) = m {
            by_mid.entry(cell_of(*m)).or_default().push(k);
        }
    }
    let close = |a: usize, b: usize| matches!((points.get(a), points.get(b)), (Some(p), Some(q)) if p.distance(*q) <= tol);
    let mut joined: Vec<usize> = (0..points.len()).collect();
    // Where two passes of one path cross another (an open path's way there and back, coincident
    // outlines), the sweep can also join its two cuts, a rounding error apart, with a stub of that
    // length: a speck of a piece, and a vertex twice over. Such stubs are no edges: their ends are
    // one vertex. The sweep moves points by a few `eps` at most up and down its frame; along a path
    // that runs nearly level in it (where the height a cut is found at pins its position down
    // least) a cut can slide further, here up to a small fraction of the precision results are
    // tidied to, below which no edge is drawn anyway. A vertex's cluster stays within those bounds
    // as a whole: a run of short, nearly level segments (a finely flattened or traced path) is real
    // geometry, not one stub after another, and must not collapse into one vertex.
    let (stub_y, stub_x) = (4.0 * eps, (DEFAULT_PRECISION / 16.0).max(4.0 * eps));
    let mut extent: Vec<kurbo::Rect> = points.iter().map(|&p| kurbo::Rect::from_points(p, p)).collect();
    for (segs, [v0, v1]) in &pieces {
        let Some(&p0) = points.get(*v0) else { continue };
        if !segs.iter().flat_map(|s| [s.c.p1, s.c.p2, s.c.p3]).all(|p| (p.y - p0.y).abs() <= stub_y && (p.x - p0.x).abs() <= stub_x) {
            continue;
        }
        let (a, b) = (find(&mut joined, *v0), find(&mut joined, *v1));
        let (Some(&ra), Some(&rb)) = (extent.get(a), extent.get(b)) else { continue };
        let both = ra.union(rb);
        if a == b || both.width() > stub_x || both.height() > stub_y {
            continue;
        }
        let (root, child) = (a.min(b), a.max(b));
        if let Some(p) = joined.get_mut(child) {
            *p = root;
        }
        if let Some(r) = extent.get_mut(root) {
            *r = both;
        }
    }
    for (k, (_, [a, b])) in pieces.iter().enumerate() {
        let Some(&Some(m)) = mids.get(k) else { continue };
        let (cx, cy) = cell_of(m);
        let others = (cx - 1..=cx + 1).flat_map(|x| (cy - 1..=cy + 1).map(move |y| (x, y))).filter_map(|c| by_mid.get(&c)).flatten();
        for &j in others.filter(|&&j| j > k) {
            let (Some((_, [c, d])), Some(&Some(n))) = (pieces.get(j), mids.get(j)) else { continue };
            if m.distance(n) > tol {
                continue;
            }
            let pairs = if close(*a, *c) && close(*b, *d) {
                [(*a, *c), (*b, *d)]
            } else if close(*a, *d) && close(*b, *c) {
                [(*a, *d), (*b, *c)]
            } else {
                continue;
            };
            for (x, y) in pairs {
                let (x, y) = (find(&mut joined, x), find(&mut joined, y));
                if let Some(r) = joined.get_mut(x.max(y)) {
                    *r = x.min(y);
                }
            }
        }
    }
    let mut edges: Vec<Edge> = Vec::new();
    let mut between: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    let outlines = Outlines::new(&inputs, eps);
    for (mut segs, ends) in pieces {
        let ends = ends.map(|v| find(&mut joined, v));
        let (Some(&p0), Some(&p1)) = (points.get(ends[0]), points.get(ends[1])) else { continue };
        if ends[0] == ends[1] {
            continue;
        }
        // Every edge at a vertex starts or ends exactly on it, so faces meeting there share its
        // coordinates: merging them (a union) then cancels their common edge, which it can't when
        // each face carries its own rounding of the corner (#893).
        pin_ends(&mut segs, p0, p1);
        let Some(source) = mid(&segs).and_then(|m| outlines.source_at(m)) else { continue };
        // Coincident pieces the sweep left apart are one edge (the front-most input's).
        let key = (ends[0].min(ends[1]), ends[0].max(ends[1]));
        let here = mid(&segs);
        let twin_of = between.get(&key).and_then(|v| {
            v.iter().copied().find(|&e| {
                let other = edges.get(e).and_then(|e| mid(&e.segs));
                matches!((here, other), (Some(a), Some(b)) if a.distance(b) < precision)
            })
        });
        if let Some(e) = twin_of {
            if let Some(e) = edges.get_mut(e)
                && source > e.source
            {
                e.source = source;
            }
            continue;
        }
        between.entry(key).or_default().push(edges.len());
        edges.push(Edge { segs, ends, source });
    }
    let nv = points.len();

    // Half-edges leaving each vertex, sorted by direction; walking a face, the next half-edge is
    // the one just clockwise of the way back, so bounded faces come out with positive area and
    // the outer boundary of each connected piece with negative area.
    let mut around: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for h in 0..2 * edges.len() {
        if let Some(list) = origin(&edges, h).and_then(|v| around.get_mut(v)) {
            list.push(h);
        }
    }
    let mut slot_of = vec![0usize; 2 * edges.len()];
    for (v, list) in around.iter_mut().enumerate() {
        let Some(&at) = points.get(v) else { continue };
        // Half the shortest chord of the edges here: every one of them gets that far from it.
        let r = 0.5 * list.iter().filter_map(|&h| far_end(&edges, h).map(|p| p.distance(at))).fold(f64::INFINITY, f64::min);
        let mut keyed: Vec<(f64, usize)> = list.iter().map(|&h| (leaving_angle(&edges, h, at, r), h)).collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        *list = keyed.into_iter().map(|(_, h)| h).collect();
        for (k, &h) in list.iter().enumerate() {
            if let Some(s) = slot_of.get_mut(h) {
                *s = k;
            }
        }
    }
    let next = |h: usize| -> Option<usize> {
        let back = twin(h);
        let list = around.get(origin(&edges, back)?)?;
        let k = *slot_of.get(back)?;
        list.get((k + list.len() - 1) % list.len()).copied()
    };

    // Connected pieces of the graph.
    let mut parent: Vec<usize> = (0..nv).collect();
    for e in &edges {
        let (a, b) = (find(&mut parent, e.ends[0]), find(&mut parent, e.ends[1]));
        if let Some(p) = parent.get_mut(a) {
            *p = b;
        }
    }
    let piece: Vec<usize> = (0..nv).map(|v| find(&mut parent, v)).collect();
    let piece_of = |cycle: &[usize]| cycle.first().and_then(|&h| origin(&edges, h)).and_then(|v| piece.get(v)).copied();

    // Walk every cycle: positive ones are faces, negative ones outer boundaries (holes in the
    // face around them, if any).
    let mut seen = vec![false; 2 * edges.len()];
    let mut faces: Vec<(Vec<usize>, BezPath, f64)> = Vec::new();
    let mut slivers: Vec<(Vec<usize>, BezPath)> = Vec::new();
    let mut holes: Vec<(Vec<usize>, BezPath)> = Vec::new();
    for start in 0..2 * edges.len() {
        if seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut cycle = vec![];
        let mut h = start;
        while let Some(s) = seen.get_mut(h).filter(|s| !**s) {
            *s = true;
            cycle.push(h);
            match next(h) {
                Some(n) => h = n,
                None => break,
            }
        }
        let cycle = without_spurs(&cycle);
        if cycle.is_empty() {
            continue;
        }
        let bp = cycle_bezpath(&edges, &cycle);
        let area = bp.area();
        if area > 0.0 && !is_sliver(&bp, tidy.precision) {
            faces.push((cycle, bp, area));
        } else if area > 0.0 {
            slivers.push((cycle, bp));
        } else if area < -(tidy.precision * tidy.precision) {
            holes.push((cycle, bp));
        }
    }

    // Each hole goes in the smallest face of another piece around it.
    let face_index = RectIndex::new(faces.iter().enumerate().map(|(i, (_, bp, _))| (bp.bounding_box(), i)).collect());
    let mut face_holes: Vec<Vec<usize>> = vec![Vec::new(); faces.len()];
    for (k, (cycle, _)) in holes.iter().enumerate() {
        let Some(p) = cycle.first().and_then(|&h| origin(&edges, h)).and_then(|v| points.get(v)).copied() else { continue };
        let own = piece_of(cycle);
        let around_it = face_index
            .at(p, 0.0)
            .filter_map(|&i| faces.get(i).map(|f| (i, f)))
            .filter(|(_, (fc, bp, _))| piece_of(fc) != own && bp.winding(p) != 0)
            .min_by(|a, b| a.1.2.total_cmp(&b.1.2))
            .map(|(i, _)| i);
        if let Some(list) = around_it.and_then(|i| face_holes.get_mut(i)) {
            list.push(k);
        }
    }

    // The inputs that cover each sliver, tested at a point inside it.
    let fills: Vec<Fill> = if slivers.is_empty() || !cover { Vec::new() } else { shapes.iter().map(Fill::new).collect() };
    let slivers: Vec<(Vec<usize>, Vec<usize>)> = slivers
        .into_iter()
        .map(|(cycle, mut bp)| {
            if !cover {
                return (cycle, Vec::new());
            }
            bp.apply_affine(UNSHEAR);
            let sources = interior_point(&PathData::from_bezpath(&bp))
                .map(|p| fills.iter().enumerate().filter(|(_, f)| f.contains(p)).map(|(i, _)| i).collect())
                .unwrap_or_default();
            (cycle, sources)
        })
        .collect();

    // The inputs' anchors, which results land on again up to the sweep's tolerance.
    let snap = 2.0 * eps;
    let mut anchors: Vec<Point> = shapes.iter().flat_map(|s| s.path.subpaths.iter().flat_map(|sp| sp.anchors.iter().map(|a| a.p))).collect();
    anchors.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    Some(Map { edges, around, slot_of, faces, face_index, slivers, holes, face_holes, tidy, anchors, snap })
}

impl Map {
    /// The union of the faces that hold `inside` (document points, one per face), traced along the
    /// map's own edges: those between two merged faces drop out exactly, where a boolean union of
    /// the faces' outlines can leave them in when its sweep rounds their shared corners apart
    /// (#893). A sliver next to a merged face that only inputs marked in `cover` fill merges with
    /// them, so it doesn't stay as a hairline gap between them. With `rest`, a sliver goes in only
    /// when its input covers it, and not when the merge of the faces merged on their own takes it
    /// in. Outer outlines come out positive, holes negative. None when a point is in no face.
    pub(crate) fn merge(&self, inside: &[Point], cover: &[bool], rest: Option<&Rest>) -> Option<PathData> {
        let halves = 2 * self.edges.len();
        // The face on each half-edge's side; holes bound the face around them. Slivers come after
        // the faces.
        let mut face_of: Vec<Option<usize>> = vec![None; halves];
        for (i, (cycle, ..)) in self.faces.iter().enumerate() {
            let hole_cycles = self.face_holes.get(i)?.iter().filter_map(|&k| self.holes.get(k)).map(|(c, _)| c);
            for &h in std::iter::once(cycle).chain(hole_cycles).flatten() {
                *face_of.get_mut(h)? = Some(i);
            }
        }
        for (k, (cycle, _)) in self.slivers.iter().enumerate() {
            for &h in cycle {
                *face_of.get_mut(h)? = Some(self.faces.len() + k);
            }
        }
        // The smallest face around a point: faces of a piece inside a hole lie within the face around.
        let face_at = |p: Point| {
            let q = SHEAR * p;
            self.face_index
                .at(q, 0.0)
                .filter_map(|&i| self.faces.get(i).map(|f| (i, f)))
                .filter(|(_, (_, bp, _))| bp.winding(q) != 0)
                .min_by(|a, b| a.1.2.total_cmp(&b.1.2))
                .map(|(i, _)| i)
        };
        let mut picked = vec![false; self.faces.len() + self.slivers.len()];
        for &p in inside {
            *picked.get_mut(face_at(p)?)? = true;
        }
        let mut other = vec![false; self.faces.len()];
        for i in rest.iter().flat_map(|r| &r.merged).filter_map(|&p| face_at(p)) {
            if let Some(o) = other.get_mut(i) {
                *o = true;
            }
        }
        let next_to = |cycle: &[usize], marked: &[bool]| {
            cycle.iter().any(|&h| face_of.get(twin(h)).copied().flatten().is_some_and(|f| f < self.faces.len() && marked.get(f) == Some(&true)))
        };
        for (k, (cycle, sources)) in self.slivers.iter().enumerate() {
            let within = |c: &[bool]| !sources.is_empty() && sources.iter().all(|&i| c.get(i) == Some(&true));
            let ours = rest.is_none_or(|r| sources.contains(&r.shape) && !(within(&r.cover) && next_to(cycle, &other)));
            if within(cover) && ours && next_to(cycle, &picked) {
                *picked.get_mut(self.faces.len() + k)? = true;
            }
        }
        let merged = |h: usize| face_of.get(h).copied().flatten().is_some_and(|f| picked.get(f) == Some(&true));
        let boundary = |h: usize| merged(h) && !merged(twin(h));
        let mut used = vec![false; halves];
        let mut subpaths = Vec::new();
        for start in 0..halves {
            if !boundary(start) || used.get(start).copied().unwrap_or(true) {
                continue;
            }
            let mut cycle = vec![];
            let mut h = start;
            while !used.get(h).copied().unwrap_or(true) {
                if let Some(u) = used.get_mut(h) {
                    *u = true;
                }
                cycle.push(h);
                // Turn from the way back as a face walk does, past the edges inside the merged
                // faces (between two of them, or a line's loose end), to the next boundary edge.
                let back = twin(h);
                let list = self.around.get(origin(&self.edges, back)?)?;
                let k = *self.slot_of.get(back)?;
                let n = list.len();
                h = (1..=n).filter_map(|j| list.get((k + n * 2 - j) % n).copied()).find(|&g| boundary(g))?;
            }
            if h != start {
                return None;
            }
            let segs: Vec<Seg> = cycle.iter().flat_map(|&h| half_segs(&self.edges, h)).collect();
            subpaths.push(segs_to_subpath(&tidy_segments(segs, true, &self.tidy), true)?);
        }
        (!subpaths.is_empty()).then(|| unshear(PathData::new(subpaths), &self.anchors, self.snap))
    }
}

fn build(shapes: &[Shape], precision: f64) -> Option<(Vec<Region>, Vec<Shape>)> {
    let m = map_with(shapes, false, precision)?;
    let Map { edges, around, faces, holes, face_holes, tidy, anchors, snap, .. } = &m;
    let snap = *snap;
    let fills: Vec<Fill> = shapes.iter().map(Fill::new).collect();
    let subpath = |cycle: &[usize]| {
        let segs: Vec<Seg> = cycle.iter().flat_map(|&h| half_segs(edges, h)).collect();
        segs_to_subpath(&tidy_segments(segs, true, tidy), true)
    };
    let regions = faces
        .iter()
        .zip(face_holes)
        .filter_map(|((cycle, _, _), hs)| {
            let mut subpaths = vec![subpath(cycle)?];
            subpaths.extend(hs.iter().filter_map(|&k| holes.get(k)).filter_map(|(c, _)| subpath(c)));
            let path = unshear(PathData::new(subpaths), anchors, snap);
            let sources =
                // A face finer than the default precision can need a finer outline to find a point in.
                interior_point(&path)
                    .or_else(|| (precision < DEFAULT_PRECISION).then(|| interior_point_within(&path, precision / 4.0)).flatten())
                    .map(|p| fills.iter().enumerate().filter(|(_, f)| f.contains(p)).map(|(i, _)| i).collect())
                    .unwrap_or_default();
            Some(Region { path, sources })
        })
        .collect();

    // Edges: chains of segments through vertices where nothing else meets.
    let degree: Vec<usize> = around.iter().map(Vec::len).collect();
    let passes = |v: usize| -> Option<(usize, usize)> {
        let list = around.get(v)?;
        let (&a, &b) = (list.first()?, list.get(1)?);
        (degree.get(v) == Some(&2) && edges.get(a / 2)?.source == edges.get(b / 2)?.source).then_some((a, b))
    };
    let mut used = vec![false; edges.len()];
    let mut chains: Vec<(Vec<Seg>, bool, usize)> = Vec::new();
    // A chain from half-edge `h0` up to the next vertex where it doesn't pass straight through;
    // closed when it comes back round to where it started.
    let walk = |h0: usize, used: &mut Vec<bool>| -> Option<(Vec<Seg>, bool, usize)> {
        let source = edges.get(h0 / 2)?.source;
        let mut segs = Vec::new();
        let mut h = h0;
        loop {
            let u = used.get_mut(h / 2)?;
            if *u {
                return Some((segs, true, source));
            }
            *u = true;
            segs.extend(half_segs(edges, h));
            let v = origin(edges, twin(h))?;
            let Some((a, b)) = passes(v) else { return Some((segs, origin(edges, h0) == Some(v), source)) };
            h = if a == twin(h) { b } else { a };
        }
    };
    for v in 0..around.len() {
        if passes(v).is_some() {
            continue;
        }
        for &h in around.get(v).map(Vec::as_slice).unwrap_or_default() {
            if !used.get(h / 2).copied().unwrap_or(true)
                && let Some(c) = walk(h, &mut used)
            {
                chains.push(c);
            }
        }
    }
    // What's left are closed loops with no junction on them.
    for e in 0..edges.len() {
        if !used.get(e).copied().unwrap_or(true)
            && let Some(c) = walk(2 * e, &mut used)
        {
            chains.push(c);
        }
    }
    let edge_shapes = chains
        .into_iter()
        .filter_map(|(segs, closed, source)| {
            let sp = segs_to_subpath(&tidy_segments(segs, closed, tidy), closed)?;
            Some(Shape::new(unshear(PathData::new(vec![sp]), anchors, snap), FillRule::NonZero, shapes.get(source)?.key))
        })
        .collect();
    Some((regions, edge_shapes))
}

/// A point strictly inside a filled path, well away from its edges where it can be (for re-finding
/// a Live Paint face after a rebuild).
pub fn interior_point(path: &PathData) -> Option<Point> {
    let bb = path.bounds()?;
    interior_point_within(path, (bb.width().max(bb.height()) * 1e-3).max(1e-4))
}

/// [`interior_point`] with the path's curves flattened to within `tolerance`.
fn interior_point_within(path: &PathData, tolerance: f64) -> Option<Point> {
    let bb = path.bounds()?;
    let bp = path.to_bezpath();
    let mut edges: Vec<(Point, Point)> = vec![];
    let (mut first, mut last) = (Point::ZERO, Point::ZERO);
    kurbo::flatten(bp.iter(), tolerance, |el| match el {
        kurbo::PathEl::MoveTo(p) => {
            if last != first {
                edges.push((last, first));
            }
            first = p;
            last = p;
        }
        kurbo::PathEl::LineTo(p) => {
            edges.push((last, p));
            last = p;
        }
        kurbo::PathEl::ClosePath => {
            if last != first {
                edges.push((last, first));
            }
            last = first;
        }
        _ => {}
    });
    if last != first {
        edges.push((last, first));
    }
    let mut best: Option<(f64, Point)> = None;
    for f in [0.5, 0.25, 0.75, 0.125, 0.375, 0.625, 0.875, 0.0625, 0.9375] {
        // A little off the round fractions, which on art drawn to a grid fall on the height of
        // some corner, where testing the point against other paths is unreliable.
        let y = bb.y0 + bb.height() * (f + 0.0061803);
        let mut xs: Vec<f64> =
            edges.iter().filter(|(a, c)| (a.y <= y) != (c.y <= y)).map(|(a, c)| a.x + (y - a.y) / (c.y - a.y) * (c.x - a.x)).collect();
        xs.sort_by(f64::total_cmp);
        for w in xs.windows(2) {
            let m = Point::new((w[0] + w[1]) / 2.0, y);
            let width = w[1] - w[0];
            if width > best.map_or(0.0, |b| b.0) && bp.winding(m) != 0 {
                best = Some((width, m));
            }
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests;
