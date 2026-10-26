//! The document canvas: rendering, rulers, navigation, pointer routing to tools and on-canvas
//! selection visuals (bounding box, anchors, handles, smart-guide style labels).

use egui::{Color32, CornerRadius, Pos2, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde_json::json;
use vectorcraft_doc::text::TextKind;
use vectorcraft_doc::{Node, NodeKind, SymbolBoxes, Unit};
use vectorcraft_geom::{Affine, BezPath, PathEl, Point, Rect};
use vectorcraft_tools::{Cursor, Mods, Overlay, PointerEvent, PointerKind};

use crate::opening;
use crate::state::View;
use crate::stylus::StylusFeed;
use crate::theme::{self, Tokens};
use crate::{CacheKey, VectorcraftApp, now_ms, widgets};

const RULER: f32 = 16.0;
/// The scroll bars' thickness along the window's right and bottom edges.
const SCROLL_BAR: f32 = 12.0;

/// Screen ↔ document mapping for one frame.
#[derive(Clone, Copy, Debug)]
pub struct Xf {
    pub rect: egui::Rect,
    pub zoom: f64,
    pub center: Point,
    /// View rotation in radians (Rotate View), clockwise on screen.
    pub rot: f64,
}

impl Xf {
    pub fn new(rect: egui::Rect, v: &View) -> Self {
        Self { rect, zoom: v.zoom, center: v.center, rot: v.rotation.to_radians() }
    }
    pub fn to_screen(&self, p: Point) -> Pos2 {
        let c = self.rect.center();
        let (dx, dy) = ((p.x - self.center.x) * self.zoom, (p.y - self.center.y) * self.zoom);
        let (sn, cs) = self.rot.sin_cos();
        pos2(c.x + (dx * cs - dy * sn) as f32, c.y + (dx * sn + dy * cs) as f32)
    }
    pub fn to_doc(&self, p: Pos2) -> Point {
        let c = self.rect.center();
        let (dx, dy) = ((p.x - c.x) as f64, (p.y - c.y) as f64);
        let (sn, cs) = self.rot.sin_cos();
        let (ux, uy) = (dx * cs + dy * sn, -dx * sn + dy * cs);
        Point::new(self.center.x + ux / self.zoom, self.center.y + uy / self.zoom)
    }
    /// Screen-space corners of a document rect (a rotated quad when the view is rotated).
    pub fn quad(&self, r: Rect) -> Vec<Pos2> {
        [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x1, r.y1), Point::new(r.x0, r.y1)].iter().map(|p| self.to_screen(*p)).collect()
    }
    /// Convert a screen-space delta to a document delta.
    pub fn delta_to_doc(&self, d: egui::Vec2) -> vectorcraft_geom::Vec2 {
        let (sn, cs) = self.rot.sin_cos();
        let (dx, dy) = (d.x as f64, d.y as f64);
        vectorcraft_geom::Vec2::new((dx * cs + dy * sn) / self.zoom, (-dx * sn + dy * cs) / self.zoom)
    }
    pub fn rect_to_screen(&self, r: Rect) -> egui::Rect {
        egui::Rect::from_two_pos(self.to_screen(Point::new(r.x0, r.y0)), self.to_screen(Point::new(r.x1, r.y1)))
    }
    /// Affine mapping document points to screen points.
    pub fn affine(&self) -> Affine {
        let c = self.rect.center();
        Affine::translate((c.x as f64, c.y as f64)) * Affine::rotate(self.rot) * Affine::scale(self.zoom) * Affine::translate(-self.center.to_vec2())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Tool,
    /// Hand tool / Space drag, or a middle-button drag with any tool (`middle`).
    Pan {
        start: Pos2,
        center: Point,
        middle: bool,
    },
    /// A Zoom tool drag without Animated Zoom: the area to zoom to.
    ZoomBox {
        start: Pos2,
    },
    /// A Zoom tool press with Animated Zoom ([`animated_zoom`]): dragged sideways it zooms about
    /// `start`, held still (for `held` seconds so far) it zooms on, else it's a click.
    ZoomScrub {
        start: Pos2,
        held: f32,
        moved: bool,
    },
    /// A Rotate View tool drag, or a Space+Shift drag with any tool. With `snaps` (the Rotate
    /// View tool), Shift rounds the angle to 15°.
    RotateView {
        start_angle: f64,
        start_rot: f64,
        snaps: bool,
    },
    /// A Selection tool move over a panel or toolbar: the art waits where it last moved on the
    /// canvas, and the panels get it ([`widgets::PanelDrag::Art`]) if it is released there. Back
    /// on the canvas the move goes on, as in Illustrator (#2826).
    Art,
    /// Fingers making a touch gesture ([`crate::touch`]): the first one's press does nothing.
    Gesture,
    /// A press in Presentation Mode: released as a click, it goes to the next artboard.
    Present,
}

/// Whether drag `d` is a press the tool holds. Every other drag (panning, zooming, turning the
/// view, a touch gesture, Presentation Mode) never reaches the tool, so its hover ends with it.
fn tool_holds(d: Drag) -> bool {
    matches!(d, Drag::Tool | Drag::Art)
}

/// What a hover depends on besides the pointer's screen position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HoverContext {
    /// The active tool: a tool switched to under a still pointer is hovered at once.
    tool: &'static str,
    /// The canvas, and the view's centre, zoom and rotation.
    canvas: egui::Rect,
    center: Point,
    zoom: f64,
    rotation: f64,
    /// The active document's id and revision (which moves with every edit, undo and selection
    /// change).
    doc: Option<(u64, u64)>,
}

/// Clear canvas interaction state before a tab transfer takes ownership of the artwork drag.
pub(crate) fn end_document_drag(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove::<Drag>(drag_id());
        d.remove::<Pos2>(drag_pos_id());
    });
}

/// Temp-memory key for the active canvas drag state.
fn drag_id() -> egui::Id {
    egui::Id::unique("canvas-drag")
}
/// Where the pointer was last seen during a press on the canvas.
fn drag_pos_id() -> egui::Id {
    egui::Id::unique("canvas-drag-pos")
}
/// The layer the canvas was last drawn on: a release over another layer is over a panel (#2826).
fn drag_layer_id() -> egui::Id {
    egui::Id::unique("canvas-drag-layer")
}
/// Whether `p` is over the canvas itself, not a panel or a floating window over it.
fn on_canvas(ui: &Ui, resp: &egui::Response, p: Pos2) -> bool {
    resp.rect.contains(p) && ui.clip_rect().contains(p) && ui.ctx().layer_id_at(p).is_none_or(|l| l == resp.layer_id)
}
/// The document point last sent to the tool as a drag during [`Drag::Tool`]; none until the press
/// has dragged.
fn tool_drag_doc_id() -> egui::Id {
    egui::Id::unique("canvas-tool-drag-doc")
}
/// The document point of the press that began [`Drag::Tool`], sent to the tool as its Down.
fn tool_press_doc_id() -> egui::Id {
    egui::Id::unique("canvas-tool-press-doc")
}
/// Whether the tool should get another drag event, and if so records `p` as the last position
/// sent: the pointer moved this frame, or its document position changed without a delta (#834).
/// egui's frame delta is zero when some Wayland compositors coalesce motion until release, and in
/// the frame the pointer comes back into the window after leaving it (`PointerGone` forgot where
/// it was), though it is somewhere new; judged on the delta alone, the preview stayed behind.
fn tool_drag_moved(ui: &Ui, xf: &Xf, p: Pos2, pointer: &egui::PointerState) -> bool {
    let at = xf.to_doc(p);
    let id = tool_drag_doc_id();
    // Before the press has dragged, the last position the tool got is the press itself: a press
    // held still is not a drag.
    let prev = ui.data(|d| d.get_temp::<Point>(id).or_else(|| d.get_temp::<Point>(tool_press_doc_id())));
    if pointer.delta() == egui::Vec2::ZERO && prev == Some(at) {
        return false;
    }
    ui.data_mut(|d| d.insert_temp(id, at));
    true
}

/// The pen pressure (0..1) of the press in progress: the force of this frame's pen or touch
/// input, else the pressure in the host's feed while a pen is the pointer that moved last
/// ([`crate::Services::stylus`]: [`StylusFeed::press`] for a new press, [`StylusFeed::held`]
/// after it), else the last one seen since the press (`pressed`: a new press, whose mouse has none
/// until a pen reports it). A mouse presses fully (1).
fn pen_pressure(ui: &Ui, app: &VectorcraftApp, pressed: bool) -> f32 {
    let stylus = app.services.stylus.as_ref();
    let id = egui::Id::unique("canvas-pressure");
    let force = ui.input(|i| {
        i.events.iter().rev().find_map(|e| match e {
            egui::Event::Touch { force: Some(f), .. } if f.is_finite() => Some(f.clamp(0.0, 1.0)),
            _ => None,
        })
    });
    let force = force.or_else(|| stylus.and_then(if pressed { StylusFeed::press } else { StylusFeed::held }));
    let p = force.or_else(|| if pressed { None } else { ui.data(|d| d.get_temp::<f32>(id)) }).unwrap_or(1.0);
    ui.data_mut(|d| d.insert_temp(id, p));
    p
}

/// The positions this frame's pointer moves passed through before its last one, each with the
/// pen force a touch event gave just before it, or the pressure the host's feed kept for that move
/// ([`StylusFeed::take_moves`], macOS), if any. The last move is the pointer's position. A mouse's
/// moves count too, with no pressure: a fast mouse stroke otherwise keeps one position a frame, a
/// sample every few pixels, too few for a low Fidelity to follow the hand (#3328).
fn frame_moves(ui: &Ui, stylus: Option<&StylusFeed>) -> Vec<(Pos2, Option<f32>)> {
    let pressures = stylus.map(StylusFeed::take_moves).unwrap_or_default();
    ui.input(|i| {
        let mut moves = vec![];
        if i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. })) {
            let mut force = None;
            for e in &i.events {
                match e {
                    egui::Event::Touch { force: Some(f), .. } if f.is_finite() && *f > 0.0 => force = Some(f.clamp(0.0, 1.0)),
                    egui::Event::PointerMoved(p) => moves.push((*p, force)),
                    _ => {}
                }
            }
        } else if !pressures.is_empty() {
            // The feed's last pressure is the last move's: pair them from the end. A lift's 0 is
            // no pressure (the press keeps its last touch).
            let points: Vec<Pos2> = i.events.iter().filter_map(|e| if let egui::Event::PointerMoved(p) = e { Some(*p) } else { None }).collect();
            let forces = pressures.iter().rev().map(|f| (*f > 0.0).then_some(*f)).chain(std::iter::repeat(None));
            moves = points.into_iter().rev().zip(forces).collect();
            moves.reverse();
        } else {
            moves = i.events.iter().filter_map(|e| if let egui::Event::PointerMoved(p) = e { Some((*p, None)) } else { None }).collect();
        }
        moves.pop();
        // A burst of events (a stalled frame) is thinned to a bounded count.
        const MAX: usize = 64;
        if moves.len() > MAX {
            let step = moves.len().div_ceil(MAX);
            moves = moves.into_iter().step_by(step).collect();
        }
        moves
    })
}

/// The moves of this frame ([`frame_moves`]) the active tool gets before the pointer's position:
/// all of them for a tool that builds a stroke from them ([`vectorcraft_engine::Session::tool_wants_samples`]), none
/// for the others, which get one drag a frame. The moves are read either way, so the host's feed
/// pairs the next frame's with its own.
fn tool_frame_moves(ui: &Ui, app: &VectorcraftApp) -> Vec<(Pos2, Option<f32>)> {
    let moves = frame_moves(ui, app.services.stylus.as_ref());
    if app.session.tool_wants_samples() { moves } else { vec![] }
}

pub fn mods(m: egui::Modifiers, space: bool) -> Mods {
    Mods { shift: m.shift, alt: m.alt, cmd: m.command, ctrl: m.ctrl, space }
}

/// Fit the view (View → Fit Artboard / Fit All / Actual Size).
pub fn fit(app: &mut VectorcraftApp, how: &str) {
    let Some(rect) = app.canvas_rect else {
        if let Some(v) = app.view_mut() {
            v.fitted = false;
        }
        return;
    };
    let current = app.view().map_or(0, |v| v.artboard);
    let Some(st) = app.session.active() else { return };
    let target = match how {
        "view.fitAll" => {
            st.doc.art_bounds().map(|a| st.doc.artboards.iter().fold(a, |r, ab| r.union(ab.rect))).or(st.doc.artboards.first().map(|a| a.rect))
        }
        // The navigator's artboard (the first if it has gone).
        _ => st.doc.artboards.get(current).or(st.doc.artboards.first()).map(|a| a.rect),
    };
    let Some(target) = target else { return };
    let zoom = if how == "view.actualSize" {
        actual_size_zoom(app.session.prefs.display_print_size)
    } else {
        // The width and height of the box round the target turned with the view, at zoom 1.
        let (sn, cs) = app.view().map_or(0.0, |v| v.rotation.to_radians()).sin_cos();
        let (w, h) = (target.width(), target.height());
        let zx = (rect.width() as f64 - 60.0) / (w * cs.abs() + h * sn.abs()).max(1.0);
        let zy = (rect.height() as f64 - 60.0) / (w * sn.abs() + h * cs.abs()).max(1.0);
        zx.min(zy).clamp(0.0313, 640.0)
    };
    let Some(v) = app.view_mut() else { return };
    v.center = target.center();
    v.fitted = true;
    v.zoom = zoom;
}

/// Paint the document canvas, complete or cached artwork, navigation and tool overlays.
pub fn show(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.available_rect_before_wrap();
    // `hidden_chars` holds the marks Show Hidden Characters kept and the text nodes they were laid
    // out from. Only the assignment right before the overlay call below puts them back. A frame
    // that does not draw the overlay drops them, for example with the option off, with the edges
    // hidden, on the Home screen or with no document open.
    let hidden_chars = std::mem::take(&mut app.canvas.hidden_chars);
    // A release ends the Image Trace panel's eye-button hold, also when the panel is no longer
    // drawn, so a later press elsewhere does not show the source image.
    if app.ui.trace_hold_source.is_some() && !ui.input(|i| i.pointer.any_down()) {
        app.ui.trace_hold_source = None;
    }
    // The IME geometry this pass, if the Type tool gets one; `ime::end_of_pass` consumes it after
    // the dialogs and panels draw.
    app.ime_caret_rect = None;
    // A document that opened, closed or became active since Home was chosen replaces it.
    if app.ui.home.is_some_and(|k| k != crate::menus::home_key(app)) {
        app.ui.home = None;
    }
    if crate::menus::home_showing(app) {
        // No IME owner this pass (`ime_caret_rect` stays None): the end-of-pass observation marks
        // the context absent and keeps any marked text as typed.
        home(app, ui, full);
        return;
    }
    if app.session.active().is_none() {
        // No document and Show The Home Screen When No Documents Are Open off: an empty window.
        ui.painter().rect_filled(full, 0.0, t.panel_darker);
        return;
    }
    // Normal Screen Mode and Full Screen Mode with Menu Bar keep the scroll bars along the right
    // and bottom edges; Full Screen and Presentation Mode have none.
    let outer = full;
    let bars = app.ui.screen_mode < 2;
    let full = if bars { egui::Rect::from_min_max(full.min, full.max - vec2(SCROLL_BAR, SCROLL_BAR)) } else { full };
    let rect = if app.ui.view.rulers && app.ui.screen_mode < 3 { egui::Rect::from_min_max(full.min + vec2(RULER, RULER), full.max) } else { full };
    app.canvas_rect = Some(rect);
    let fitted = app.view().is_some_and(|v| v.fitted);
    if !fitted {
        fit(app, "view.fitArtboard");
    }
    // Before the canvas's own interaction, so the view the canvas draws is the one they moved to.
    if bars {
        scroll_bars(app, ui, rect, outer, &t);
    }
    let resp = ui.interact(rect, egui::Id::unique("canvas"), Sense::click_and_drag());
    // A press on the canvas while the context menu is open only closes it (a drag too, which egui
    // alone would leave open); the tool doesn't get it.
    if app.ui.dialog.as_ref().is_some_and(crate::dialogs::revolve_gizmo::active) {
        // The artboard's 3D gizmo owns these gestures while its options are open.
        // Do not let a ring drag also move or deselect the editable source path.
    } else if resp.context_menu_opened() && resp.hovered() && ui.input(|i| i.pointer.any_pressed()) {
        egui::Popup::close_all(ui.ctx());
    } else {
        handle_input(app, ui, &resp, rect);
        // `hover_doc` is the document point under the pointer in the view `handle_input` left. The
        // wheel, a pan, a Rotate View drag, the Zoom tool and a Presentation Mode click move the view
        // there.
        let moved = Xf::new(rect, app.view().unwrap_or(&View::default()));
        app.hover_doc = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)).map(|p| moved.to_doc(p));
    }
    let v = *app.view().unwrap_or(&View::default());
    let xf = Xf::new(rect, &v);
    // While the view pans, zooms or turns under the pointer, the art under it isn't hit-tested.
    let wait = hover_wait(&mut app.canvas.view_moved, ViewKey { rect, zoom: v.zoom, center: v.center, rotation: v.rotation }, ui.input(|i| i.time));
    if let Some(secs) = wait {
        ui.ctx().request_repaint_after_secs(secs as f32);
    }
    let hover_still = wait.is_none();
    let origin_preview = if app.ui.view.rulers && app.ui.screen_mode < 3 { ruler_input(app, ui, full, rect, &xf) } else { None };
    crate::document_drag::drop_on_canvas(app, &resp, &xf);
    panel_drop(app, ui, &resp, &xf);
    // In Presentation Mode a right click or a long touch opens no context menu.
    if app.ui.screen_mode < 3 {
        context_menu(app, &resp, &xf);
    }
    let painter = ui.painter_at(rect);
    let Some(st) = app.session.active() else { return };
    let doc = st.doc.clone();
    let mask_view = st.shown_mask();

    // Pasteboard, artboard shadows and paper (Document Setup: the transparency grid's look, the
    // simulated paper colour; a white Background Contents hides the grid).
    // An artboard with a background color draws its paper in that color. With Simulate Colored Paper
    // on, the canvas multiplies it with the paper color: each sRGB channel is artboard × paper / 255,
    // rounded. The transparency grid, while it shows, covers it as it covers the others.
    painter.rect_filled(rect, 0.0, t.pasteboard);
    if app.ui.view.artboards && !app.ui.view.outline {
        let setup = &doc.setup;
        let grid = (st.transparency_grid && setup.background == vectorcraft_doc::Background::Transparent).then(|| checker_texture(ui.ctx(), setup));
        let paper = if setup.simulate_paper { crate::panels::c32(&setup.paper()) } else { Color32::WHITE };
        for ab in &doc.artboards {
            let q = xf.quad(ab.rect);
            // Hard 2 pt drop shadow, right and bottom (measured: #4d4d4d then #565656 on #606060).
            let shift = |d: f32| q.iter().map(|p| *p + vec2(d, d)).collect::<Vec<_>>();
            painter.add(Shape::convex_polygon(shift(2.0), Color32::from_black_alpha(26), Stroke::NONE));
            painter.add(Shape::convex_polygon(shift(1.0), Color32::from_black_alpha(52), Stroke::NONE));
            match &grid {
                Some((tex, cell)) => checker(&painter, &q, ab.rect, xf.zoom, tex.id(), *cell),
                None => {
                    let fill = ab.color.map_or(paper, |c| crate::panels::c32(&c) * paper);
                    painter.add(Shape::convex_polygon(q, fill, Stroke::NONE));
                }
            }
        }
    } else if app.ui.view.outline {
        for ab in &doc.artboards {
            painter.add(Shape::convex_polygon(xf.quad(ab.rect), Color32::WHITE, Stroke::NONE));
        }
    }
    // The grid behind the art (Guides & Grid › Grids In Back), else over it below.
    let grid_look = app.ui.view.grid.then(|| LineLook::grid(&app.session.prefs));
    let grids_in_back = app.session.prefs.grids_in_back;
    if let Some(look) = grid_look.filter(|_| grids_in_back) {
        grid(&painter, &xf, doc.grid.spacing, doc.grid.subdivisions, look);
    }

    // Artwork raster.
    let ppp = ui.ctx().pixels_per_point();
    // Match Renderer::render_as before recording the cache extent or building its view.
    let (w, h) = (
        ((rect.width() * ppp).round().max(1.0) as u32).min(u32::from(u16::MAX)),
        ((rect.height() * ppp).round().max(1.0) as u32).min(u32::from(u16::MAX)),
    );
    // View › Pixel Preview: the art as it rasterizes, one pixel per point, while a document pixel
    // is bigger than a screen pixel (below that the screen render already shows it).
    let pixel = (app.ui.view.pixel_preview && v.zoom * ppp as f64 > 1.0).then(|| pixel_region(&xf)).flatten();
    // File Handling › Display Bitmaps as Anti-aliased Images in Pixel Preview: off, images show
    // their pixels as they rasterize, hard-edged.
    let smooth_images = pixel.is_none() || app.session.prefs.anti_aliased_bitmaps;
    // Paths made into guides are drawn as the ruler guides are (Preferences › Guides & Grid).
    let guide_look = LineLook::guides(&app.session.prefs, &t);
    let key = CacheKey {
        guide: ([guide_look.color.r(), guide_look.color.g(), guide_look.color.b()], guide_look.dots),
        doc: st.uid as usize,
        revision: st.revision,
        interacting: st.interaction.is_some(),
        zoom: v.zoom,
        cx: v.center.x,
        cy: v.center.y,
        w,
        h,
        outline: app.ui.view.outline,
        trim: app.ui.view.trim_view,
        templates: app.ui.view.templates,
        guides: app.ui.view.guides,
        ppp,
        hidden: vec![],
        rot: v.rotation,
        anti_alias: app.session.prefs.anti_aliased_artwork,
        highlight_alternate_glyphs: app.session.prefs.highlight_alternate_glyphs,
        highlight_substituted_fonts: app.session.prefs.highlight_substituted_fonts,
        placed: vectorcraft_render::placed_document::generation(),
        pixel,
        smooth_images,
        isolated: st.isolation.map(|id| id.0),
        // Appearance of Black › On Screen: Display All Blacks as Rich Black.
        rich_black: app.session.prefs.black_on_screen == "rich",
        // A tool's drag on the canvas, during an interaction (not a dialog's preview).
        dragging: app.session.in_interaction() && ui.data(|d| d.get_temp::<Drag>(drag_id())) == Some(Drag::Tool),
        // The Image Trace panel's eye button, while it is held.
        trace_source: app.ui.trace_hold_source.clone(),
    };
    // Placed documents' bitmaps are being made: draw again when they are ready.
    if vectorcraft_render::placed_document::busy() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }
    if !app.canvas.worker_started {
        app.canvas.worker_started = true;
        if std::env::var_os("VECTORCRAFT_SYNC_RENDER").is_none() {
            app.canvas.worker = crate::render_worker::Worker::spawn(ui.ctx().clone());
        }
    }
    // Upload finished background renders.
    if let Some(done) = app.canvas.worker.as_mut().and_then(|w| w.poll())
        && done.key.doc == key.doc
    {
        upload(app, ui.ctx(), &done.img, done.key.pixel.is_some());
        app.canvas.key = Some(done.key);
        app.canvas.shown = Some((done.doc, done.opts));
        app.perf.render_ms = done.ms;
        app.canvas.last_ms = done.ms;
        app.canvas.moved = done.moved > 0;
        app.canvas.from_worker = true;
    }
    // With `waits_for_still` set (the web build, which has no render worker), a heavy document is
    // not rendered while only the view changes: the frame shown is drawn reprojected below, and the
    // first frame after the view has been still for STILL_SECS renders.
    let wait = if app.canvas.waits_for_still {
        let slow = app.canvas.last_ms > 8.0 && app.canvas.texture.is_some();
        still_wait(&mut app.canvas.still, app.canvas.key.as_ref(), &key, slow, ui.input(|i| i.time))
    } else {
        None
    };
    if let Some(secs) = wait {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(secs));
    }
    if wait.is_none() && (app.canvas.key.as_ref() != Some(&key) || app.canvas.texture.is_none()) {
        let (w, h, view) = match pixel {
            Some([x0, y0, x1, y1]) => ((x1 - x0) as u32, (y1 - y0) as u32, Affine::translate((-x0 as f64, -y0 as f64))),
            None => (
                w,
                h,
                Affine::translate((w as f64 / 2.0, h as f64 / 2.0))
                    * Affine::rotate(v.rotation.to_radians())
                    * Affine::scale(v.zoom * ppp as f64)
                    * Affine::translate(-v.center.to_vec2()),
            ),
        };
        let opts = vectorcraft_render::RenderOptions { outline: app.ui.view.outline, background: None, artboards: false, ..Default::default() };
        let opts = vectorcraft_render::RenderOptions {
            proof: vectorcraft_render::proof::active_proof(),
            overprint_preview: vectorcraft_render::proof::overprint_preview_on(),
            trim: app.ui.view.trim_view,
            hide_templates: !app.ui.view.templates,
            hide_guides: !app.ui.view.guides,
            tile_edge: vectorcraft_color::Color::from_hex(&app.session.prefs.pattern_tile_edge_color).map_or(opts.tile_edge, |c| {
                let [r, g, b, _] = c.to_rgba8(1.0);
                [r, g, b]
            }),
            guide: key.guide,
            mask_view,
            highlight_substitutions: true,
            // Type › Highlight Alternate Glyphs (#394): pink behind OpenType substitutions.
            highlight_alternate_glyphs: app.session.prefs.highlight_alternate_glyphs,
            // Type › Highlight Substituted Fonts: pink behind text in a substitute font.
            highlight_substituted_fonts: app.session.prefs.highlight_substituted_fonts,
            // General › Anti-aliased Artwork: off, edges are hard on screen (raster effects and
            // pattern tiles stay smooth), as in Illustrator.
            anti_alias: if app.session.prefs.anti_aliased_artwork { vectorcraft_render::AntiAlias::Art } else { vectorcraft_render::AntiAlias::None },
            progressive_placed: true,
            trace_views: true,
            // Image Trace › the eye button held: the source image of the selected objects.
            trace_source: key.trace_source.clone(),
            smooth_images,
            // Isolation mode: the art around the isolated group or layer is dimmed (#833).
            isolated: key.isolated.map(vectorcraft_doc::NodeId),
            // Images far smaller on screen than they are sample their mip levels (#1244).
            image_mips: true,
            // An image whose pixels can't be decoded shows as a crossed box, and the status message
            // below reports it.
            mark_undecoded_images: true,
            // During a drag or a dialog's preview, objects that only moved draw their raster
            // effects from the cached rasters of an earlier frame (#1257).
            interacting: key.interacting,
            // During a tool's drag, raster effects without a cached raster to draw render at a
            // lower resolution (#2558); the frame after the drag renders them as a still frame does.
            dragging: key.dragging,
            rich_black: key.rich_black,
            ..opts
        };
        // Light documents render synchronously (no lag vs overlays); heavy ones go to the worker.
        // A document switch renders synchronously so another document's frame is never shown.
        // The first frame after an interaction, when the frame shown drew moved rasters, also goes
        // to the worker. An interaction's frames stay on the worker once it drew the frame shown:
        // its renderer keeps the rasters of the art's effects that make those frames fast, and this
        // thread's renderer has none, so a frame fast on the worker would be slow here.
        let same_doc = app.canvas.key.as_ref().is_some_and(|k| k.doc == key.doc);
        let settling = app.canvas.moved && !key.interacting;
        let staying = key.interacting && app.canvas.from_worker;
        let heavy = (app.canvas.last_ms > 8.0 || settling || staying) && app.canvas.texture.is_some() && same_doc;
        let changed = changed_pixels(app, &key, &doc, &opts, view, w, h);
        match (&mut app.canvas.worker, heavy, changed) {
            // Only the document changed since the frame shown, in a part of it: that part is
            // rendered again, here, into the texture. `last_ms` stays the whole frame's.
            (_, _, Some(region)) => {
                let t0 = now_ms();
                if let (Some([x0, y0, x1, y1]), Some(tex)) = (region, &mut app.canvas.texture) {
                    let part = app.canvas.renderer.render(&doc, x1 - x0, y1 - y0, Affine::translate((-f64::from(x0), -f64::from(y0))) * view, &opts);
                    let color = color_image(&part);
                    let options = if pixel.is_some() { egui::TextureOptions::NEAREST } else { egui::TextureOptions::LINEAR };
                    tex.set_partial([x0 as usize, y0 as usize], color, options);
                    app.canvas.moved |= app.canvas.renderer.stats.moved > 0;
                }
                app.canvas.key = Some(key.clone());
                app.canvas.shown = Some((doc.clone(), opts.clone()));
                app.perf.render_ms = now_ms() - t0;
            }
            (Some(worker), true, None) => worker.submit(crate::render_worker::Job { key: key.clone(), doc: doc.clone(), w, h, view, opts }),
            _ => {
                let t0 = now_ms();
                let img = app.canvas.renderer.render(&doc, w, h, view, &opts);
                upload(app, ui.ctx(), &img, pixel.is_some());
                app.canvas.key = Some(key.clone());
                app.canvas.shown = Some((doc.clone(), opts.clone()));
                app.perf.render_ms = now_ms() - t0;
                app.canvas.last_ms = app.perf.render_ms;
                app.canvas.moved = app.canvas.renderer.stats.moved > 0;
                app.canvas.from_worker = false;
            }
        }
    }
    // The status bar reports each image that a renderer (this thread's or the worker's) could not
    // decode, once per image in the process.
    let undecoded = vectorcraft_render::newly_undecodable(doc.images.values());
    if undecoded > 0 {
        app.status(format!("{undecoded} image(s) couldn't be decoded and show as crossed boxes"));
    }
    if let (Some(tex), Some(k)) = (&app.canvas.texture, &app.canvas.key)
        && k.doc == key.doc
    {
        // The frame being shown covers the canvas rect it was rendered for. While a panel is
        // dragged that rect is a different size, so reprojecting the frame through the *current*
        // rect stretches the art to the new size until the worker's frame arrives, then it snaps
        // back. Rebuild the rect it was rendered for from the key's own pixel size instead.
        let old_rect = {
            let ppp = if k.ppp > 0.0 { k.ppp } else { 1.0 };
            egui::Rect::from_center_size(rect.center(), vec2(k.w as f32 / ppp, k.h as f32 / ppp))
        };
        match k.pixel {
            // Document pixels: placed over the document rect they cover, turning with the view.
            Some([x0, y0, x1, y1]) => {
                textured_quad(&painter, tex.id(), &xf.quad(Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64)), (1.0, 1.0));
            }
            None if (k.rot - v.rotation).abs() < 1e-9 => {
                // Reproject the last frame if it was rendered for a different view.
                let old = Xf { rect: old_rect, zoom: k.zoom, center: Point::new(k.cx, k.cy), rot: xf.rot };
                let a = xf.to_screen(old.to_doc(old_rect.min));
                let b = xf.to_screen(old.to_doc(old_rect.max));
                painter.image(tex.id(), egui::Rect::from_min_max(a, b), egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            // The view turned after the last frame was rendered, and the worker renders the frame at
            // the new angle. Until it arrives, the last frame is drawn turned into this view: each
            // corner goes where this view puts the document point that the corner showed.
            None => {
                let old = Xf { rect: old_rect, zoom: k.zoom, center: Point::new(k.cx, k.cy), rot: k.rot.to_radians() };
                let q =
                    [old_rect.left_top(), old_rect.right_top(), old_rect.right_bottom(), old_rect.left_bottom()].map(|c| xf.to_screen(old.to_doc(c)));
                textured_quad(&painter, tex.id(), &q, (1.0, 1.0));
            }
        }
    }
    if let Some(look) = grid_look.filter(|_| !grids_in_back) {
        grid(&painter, &xf, doc.grid.spacing, doc.grid.subdivisions, look);
    }
    // The pixel grid (Guides & Grid › Show Pixel Grid (Above 600% Zoom)): in Pixel Preview from
    // 600% zoom, a line at every document pixel over the art, so the pixels it rasterizes to show.
    if app.ui.view.pixel_preview && v.zoom >= PIXEL_GRID_ZOOM && app.session.prefs.show_pixel_grid {
        grid(&painter, &xf, 1.0, 1, LineLook { color: PIXEL_GRID, dots: false });
    }
    // Artboard edges and names. The Artboard tool labels its active artboard itself (following
    // a move or resize): that one isn't named twice.
    let active_ab = crate::panels::artboards::selected(app, doc.artboards.len());
    let artboard_tool = app.session.tool_id() == "artboard";
    let tool_labelled = artboard_tool.then(|| app.session.tool_options()["active"].as_u64()).flatten();
    let editing_name = ui.data(|d| d.get_temp::<ArtboardNameEdit>(name_edit_id()).map(|e| e.index));
    for (i, ab) in doc.artboards.iter().enumerate() {
        let c = if i == active_ab { Color32::from_gray(0) } else { Color32::from_gray(120) };
        painter.add(Shape::closed_line(xf.quad(ab.rect), Stroke::new(if i == active_ab { 1.0 } else { 0.6 }, c)));
        // The bleed (Document Setup) as a red outline around the artboard.
        if doc.setup.has_bleed() {
            painter.add(Shape::closed_line(xf.quad(doc.setup.bleed_rect(ab.rect)), Stroke::new(1.0, t.bleed)));
        }
        // Show Center Mark, Show Cross Hairs and Show Video Safe Areas (Artboard Options). Only the
        // canvas draws them, as it does the edges: exports and print leave them out.
        artboard_marks(&painter, &xf, ab, t.artboard_mark);
        if editing_name != Some(i) && (artboard_tool || doc.artboards.len() > 1) && tool_labelled != u64::try_from(i).ok() {
            // At the artboard's own top-left corner, which a rotated view turns away from its
            // bounding box's (#1095).
            let corner = xf.to_screen(Point::new(ab.rect.x0, ab.rect.y0));
            artboard_label(&painter, corner, (ab.rect.width() * xf.zoom) as f32, &format!("{:02} - {}", i + 1, ab.name), t.text_dim);
        }
    }
    if app.ui.view.guides {
        let look = LineLook::guides(&app.session.prefs, &t);
        // Selected guides in the selection colour.
        let picked = LineLook { color: t.selection, ..look };
        let selected: &[usize] = app.session.active().map_or(&[], |st| &st.selection.guides);
        let key = app.session.active().and_then(|st| st.selection.key_guide);
        let view = view_bounds(&xf);
        // Guides on a hidden layer hide with it.
        for (i, g) in doc.guides.iter().enumerate().filter(|(_, g)| doc.guide_shown(g)) {
            let at = |along: f64| xf.to_screen(if g.vertical { Point::new(g.pos, along) } else { Point::new(along, g.pos) });
            // An artboard guide runs across its artboard, a canvas guide across the window (the part
            // of its document line that the canvas shows, turned with the view).
            let (a, b) = match doc.guide_span(g) {
                Some((from, to)) => (at(from), at(to)),
                None => {
                    let (from, to) = if g.vertical { (view.y0, view.y1) } else { (view.x0, view.x1) };
                    let Some(ends) = clip_segment(at(from), at(to), rect) else { continue };
                    ends
                }
            };
            if selected.contains(&i) { picked } else { look }.guide(&painter, a, b);
            // The key guide (Align to Key Object, #2143): thicker, as a key object's outline is.
            if key == Some(i) {
                painter.line_segment([a, b], Stroke::new(KEY_OUTLINE, picked.color));
            }
        }
    }

    if !app.session.slices_hidden() {
        slice_overlay(app, &painter, &xf);
    }
    if app.session.active().is_some_and(|d| d.print_tiling) || app.session.tool_id() == "printTiling" {
        print_tiling_overlay(app, &painter, &xf);
    }
    original_path_overlay(app, &painter, &xf);

    // Selection visuals and tool overlays.
    let revolve_edit = app.ui.dialog.as_ref().is_some_and(crate::dialogs::revolve_gizmo::active);
    let view_info = app.view_info();
    // View → Hide Gradient Annotator hides the Gradient tool's annotator.
    let mut overlays = if !revolve_edit && (app.session.tool_id() != "gradient" || app.ui.view.gradient_annotator) {
        app.session.overlays(view_info)
    } else {
        vec![]
    };
    // Gap Options' Preview shows the gaps it finds.
    overlays.extend(crate::dialogs::live_paint_gaps::overlays(app));
    if app.ui.view.edges && !revolve_edit {
        if hover_still {
            hover_highlight(app, &painter, &xf);
        }
        selection_overlay(app, &painter, &xf, overlays.iter().any(|o| matches!(o, Overlay::CurvePoint { .. })));
        mirror_axis_overlay(app, &painter, &xf);
        grid_repeat_overlay(app, &painter, &xf);
        if app.ui.view.text_threads {
            // Under the tools' overlays: a selected frame's own ports are drawn over its thread's.
            let mut threads = thread_overlays(app, xf.zoom);
            threads.append(&mut overlays);
            overlays = threads;
        }
        if app.ui.view.live_paint_gaps {
            overlays.extend(live_paint_gap_overlays(app));
        }
        if app.ui.view.hidden_chars {
            app.canvas.hidden_chars = hidden_chars;
            hidden_chars_overlay(app, &painter, &xf);
        }
    }
    draw_overlays(&painter, &xf, &overlays, &t, HandleLook::of(&app.session.prefs));
    if !revolve_edit {
        shaper_widget_overlay(app, &painter, &xf);
    }
    ime_geometry(app, ui.ctx(), &xf);
    crate::place::paint_drop_highlight(app, ui.ctx(), &painter, rect);
    // A Rotate View drag (the tool, or Space+Shift with any tool) shows the view's angle on a dial
    // at the canvas center, the point the view turns about.
    if app.ui.screen_mode < 3 && matches!(ui.data(|d| d.get_temp::<Drag>(drag_id())), Some(Drag::RotateView { .. })) {
        rotate_view_dial(&painter, rect.center(), v.rotation, &t);
    }

    if app.ui.view.rulers && app.ui.screen_mode < 3 {
        if let Some(origin) = origin_preview {
            let at = xf.to_screen(origin);
            painter.line_segment([pos2(at.x, rect.top()), pos2(at.x, rect.bottom())], Stroke::new(1.0, t.text_dim));
            painter.line_segment([pos2(rect.left(), at.y), pos2(rect.right(), at.y)], Stroke::new(1.0, t.text_dim));
        }
        // View › Rulers › Show Video Rulers counts the active artboard in video pixels.
        let video = app.ui.view.video_rulers.then(|| crate::dialogs::artboard_options::active_index(app));
        let scale = match video.and_then(|i| app.session.active()?.doc.artboards.get(i)) {
            Some(ab) => RulerScale::video(ab),
            None => RulerScale::document(app.session.general_unit(), origin_preview.unwrap_or_else(|| app.ruler_origin())),
        };
        rulers(ui, full, &xf, app.hover_doc, scale, &t);
    }
    // The task bar is not drawn while the Simplify slider shows.
    if app.ui.task_bar && !revolve_edit && !app.session.tool_busy() && app.ui.screen_mode < 3 && !crate::dialogs::simplify_slider::showing(app) {
        task_bar(app, ui, &xf);
    }
    if app.ui.screen_mode < 3 {
        crate::free_transform::show(app, ui, xf.rect);
    }
    artboard_name_editor(app, ui, &xf);
    // Cursor.
    if resp.hovered() {
        if opening::long_operation_busy(app) {
            ui.ctx().set_cursor_icon(cursor_icon(Cursor::Busy));
            return;
        }
        let m = ui.input(|i| i.modifiers);
        let space = ui.input(|i| i.key_down(egui::Key::Space));
        let drag = ui.data(|d| d.get_temp::<Drag>(drag_id()));
        let panning = matches!(drag, Some(Drag::Pan { .. }));
        let zooming = if space { m.command } else { app.session.tool_id() == "zoom" };
        // A rotating drag keeps its cursor when Shift is let go.
        let rotating =
            matches!(drag, Some(Drag::RotateView { .. })) || if space { m.shift && !m.command } else { app.session.tool_id() == "rotateView" };
        // The hand and the magnifier are drawn like the tool cursors below: Windows has no
        // system cursor for them (winit shows the four-headed move cursor for Grab).
        let view_cursor = if panning {
            Some(Cursor::HandGrab)
        } else if rotating {
            Some(Cursor::Rotate)
        } else if zooming {
            Some(if m.alt { Cursor::ZoomOut } else { Cursor::ZoomIn })
        } else if space || app.session.tool_id() == "hand" {
            Some(Cursor::Hand)
        } else {
            None
        };
        // In Presentation Mode the pointer is the default one, with no tool cursor.
        let tool_cursor = (app.ui.screen_mode < 3).then(|| view_cursor.or_else(|| hover_cursor(app, mods(m, space), view_info, hover_still)));
        let cur = if let Some(c) = tool_cursor.flatten() {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("tool-cursor")));
            // The loaded place cursor carries the file's thumbnail.
            if app.session.tool_id() == "place"
                && let Some(hp) = ui.input(|i| i.pointer.hover_pos())
            {
                crate::place::paint_cursor(app, ui.ctx(), &painter, hp);
            }
            // The glyph cursors are drawn here; General › Use Precise Cursors (the session turns the drawing tools' pointers
            // into crosshairs, `Cursor::precise`) needs no handling. VECTORCRAFT_SYSTEM_CURSORS opts out of the glyphs.
            let custom = std::env::var_os("VECTORCRAFT_SYSTEM_CURSORS").is_none();
            // User Interface › Scale Cursor Proportional to UI: the glyphs grow with UI Scaling;
            // off, they keep their size.
            let ui_scale = if app.session.prefs.scale_cursor_with_ui { 1.0 } else { ui.ctx().zoom_factor() };
            match ui.input(|i| i.pointer.hover_pos()) {
                // An OS cursor: the system moves it at once, where a painted one trails the pointer
                // (#444). The system cursor stands in for cursors without a glyph. Over a window
                // without the focus, macOS shows its own pointer whichever cursor the window sets,
                // and the glyph is not painted there.
                Some(_) if custom && crate::cursors::os_cursors(crate::cursors::OS_CURSORS, app.services.wayland) => {
                    let ppp = ui.ctx().pixels_per_point() / ui_scale;
                    ui.ctx().set_cursor_image(app.canvas.cursors.get(c, ppp));
                    cursor_icon(c)
                }
                Some(hp)
                    if custom
                        && crate::cursors::paint_glyphs(crate::cursors::HIDES_ONLY_WHEN_KEY, ui.input(|i| i.focused))
                        && crate::cursors::paint(&painter, c, hp, 1.0 / ui_scale) =>
                {
                    // No cursor theme (Wayland): the glyph is painted, the system pointer stays (#2450).
                    if crate::cursors::hide_system_pointer(app.services.no_cursor_theme) { egui::CursorIcon::None } else { cursor_icon(c) }
                }
                _ => cursor_icon(c),
            }
        } else {
            egui::CursorIcon::Default
        };
        ui.ctx().set_cursor_icon(cur);
    }
}

/// The active tool's cursor at the pointer, as `mods` make it. While the view moves (`still`
/// false), the cursor the tool last gave for the same tool and keys stays: asking the tool again
/// would hit-test the art under the pointer at every frame of the move (#2452).
fn hover_cursor(app: &mut VectorcraftApp, mods: Mods, view: vectorcraft_engine::ViewInfo, still: bool) -> Option<Cursor> {
    let id = app.session.tool_id();
    if !still
        && let Some((was, with, c)) = app.canvas.hover_cursor
        && (was, with) == (id, mods)
    {
        return Some(c);
    }
    let c = app.session.cursor(app.hover_doc?, mods, view);
    app.canvas.hover_cursor = Some((id, mods, c));
    Some(c)
}

/// How long (seconds) the view must stay still before a canvas with
/// [`crate::CanvasCache::waits_for_still`] set renders a heavy document again.
pub const STILL_SECS: f64 = 0.2;

/// How long (seconds) the view must stay still before the canvas hit-tests the art under the
/// pointer again, for Object Highlighting and the tool's cursor. While the view pans, zooms or
/// turns under a resting pointer, the document point under it changes every frame, and hit-testing
/// detailed art each time slowed every frame of the move (#2452).
pub const HOVER_STILL_SECS: f64 = 0.15;

/// The view [`hover_wait`] watches: the canvas's rectangle and the view's zoom, centre and rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewKey {
    rect: egui::Rect,
    zoom: f64,
    center: Point,
    rotation: f64,
}

/// Note the view `key` at time `now` (seconds, egui's input time) → how many more seconds the
/// canvas leaves the art under the pointer alone: the view changed less than [`HOVER_STILL_SECS`]
/// ago. The first view noted counts as still.
fn hover_wait(moved: &mut Option<(ViewKey, f64)>, key: ViewKey, now: f64) -> Option<f64> {
    match moved {
        Some((k, at)) if *k != key => (*k, *at) = (key, now),
        Some(_) => {}
        None => *moved = Some((key, f64::NEG_INFINITY)),
    }
    let waited = moved.map_or(f64::INFINITY, |(_, at)| now - at);
    // The clock went back (a new egui context): don't wait for it to catch up.
    (0.0..HOVER_STILL_SECS).contains(&waited).then_some(HOVER_STILL_SECS - waited)
}

/// Note that the canvas asks for `key` at time `now` (seconds, egui's input time) → how many more
/// seconds it draws the frame `shown` reprojected instead of rendering `key`: while the last
/// render was `slow`, only the view differs between the two, and the canvas has asked for `key`
/// for less than [`STILL_SECS`]. `still` holds the key last asked for and when the canvas first
/// asked for it. None: the canvas renders `key` now, unless it is the frame shown.
fn still_wait(still: &mut Option<(CacheKey, f64)>, shown: Option<&CacheKey>, key: &CacheKey, slow: bool, now: f64) -> Option<f64> {
    if still.as_ref().is_none_or(|(k, _)| k != key) {
        *still = Some((key.clone(), now));
    }
    let waited = still.as_ref().map_or(0.0, |(_, at)| now - at);
    // The clock went back (a new egui context): don't wait for it to catch up.
    let waiting = (0.0..STILL_SECS).contains(&waited);
    (slow && waiting && shown.is_some_and(|s| s != key && only_view_differs(s, key))).then_some(STILL_SECS - waited)
}

/// Whether two cache keys differ only in the view (zoom, center, rotation, canvas size, pixel
/// density and Pixel Preview's document pixels), with Pixel Preview on in both or off in both.
fn only_view_differs(a: &CacheKey, b: &CacheKey) -> bool {
    a.pixel.is_some() == b.pixel.is_some()
        && *a == CacheKey { zoom: a.zoom, cx: a.cx, cy: a.cy, w: a.w, h: a.h, ppp: a.ppp, rot: a.rot, pixel: a.pixel, ..b.clone() }
}

fn cursor_icon(c: Cursor) -> egui::CursorIcon {
    use egui::CursorIcon as C;
    match c {
        Cursor::Arrow
        | Cursor::ArrowHollow
        | Cursor::DirectAnchor
        | Cursor::DirectAnchorSelected
        | Cursor::ReshapeSegment
        | Cursor::GroupSelection
        | Cursor::CornerRadius
        | Cursor::PathBracket
        | Cursor::TypeWidget
        | Cursor::LoadedText => C::Default,
        Cursor::SelectionCopy => C::Copy,
        Cursor::Move => C::Move,
        Cursor::Crosshair => C::Crosshair,
        Cursor::ResizeH | Cursor::ShearH => C::ResizeHorizontal,
        Cursor::ResizeV | Cursor::ShearV => C::ResizeVertical,
        Cursor::ResizeNwSe => C::ResizeNwSe,
        Cursor::ResizeNeSw => C::ResizeNeSw,
        Cursor::Rotate | Cursor::RotateToward(_) => C::Alias,
        Cursor::Pen
        | Cursor::PenDrawing
        | Cursor::PenAdd
        | Cursor::PenDelete
        | Cursor::PenClose
        | Cursor::PenContinue
        | Cursor::PenJoin
        | Cursor::PenConvert
        | Cursor::AnchorPoint
        | Cursor::Curvature => C::Crosshair,
        Cursor::Text => C::Text,
        Cursor::Hand => C::Grab,
        Cursor::HandGrab => C::Grabbing,
        Cursor::ZoomIn => C::ZoomIn,
        Cursor::ZoomOut => C::ZoomOut,
        Cursor::Eyedropper => C::Crosshair,
        Cursor::NotAllowed => C::NotAllowed,
        Cursor::Busy => C::Progress,
        Cursor::AddStop => C::Copy,
        Cursor::RemoveStop => C::NotAllowed,
        Cursor::Slice => C::Crosshair,
        Cursor::SliceSelect => C::Default,
        Cursor::Width | Cursor::WidthAdd => C::Crosshair,
        Cursor::WidthPoint => C::Move,
        Cursor::PencilNew => C::Crosshair,
        Cursor::Blend | Cursor::BlendObject | Cursor::BlendAnchor | Cursor::ShapeBuilder | Cursor::ShapeBuilderErase => C::Crosshair,
        Cursor::PencilClose | Cursor::PencilStraight | Cursor::PencilContinue | Cursor::PaintbrushClose => C::Crosshair,
        Cursor::TypeVertical => C::VerticalText,
        // Not drawn yet: the cursor each borrows.
        Cursor::Lasso
        | Cursor::MagicWand
        | Cursor::Pencil
        | Cursor::Paintbrush
        | Cursor::BlobBrush
        | Cursor::Eraser
        | Cursor::Smooth
        | Cursor::PathEraser
        | Cursor::Join
        | Cursor::Knife
        | Cursor::Shaper
        | Cursor::LivePaintBucket
        | Cursor::Measure
        | Cursor::PrintTiling
        | Cursor::SymbolSprayer
        | Cursor::SymbolShifter
        | Cursor::SymbolScruncher
        | Cursor::SymbolSizer
        | Cursor::SymbolSpinner
        | Cursor::SymbolStainer
        | Cursor::SymbolScreener
        | Cursor::SymbolStyler
        | Cursor::ObjectsOnPath
        | Cursor::Scissors
        | Cursor::ScissorsNotAllowed
        | Cursor::MeshAdd
        | Cursor::PuppetPin => cursor_icon(c.stand_in().unwrap_or(Cursor::Crosshair)),
    }
}

/// Dispatch canvas pointer, keyboard and drop events through the active document view.
fn handle_input(app: &mut VectorcraftApp, ui: &Ui, resp: &egui::Response, rect: egui::Rect) {
    let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
    let wheel_zooms = app.session.prefs.zoom_with_mouse_wheel;
    let turn_id = egui::Id::unique("canvas-wheel-turn");
    let turn_before: WheelTurn = ui.data(|d| d.get_temp(turn_id)).unwrap_or_default();
    let mut turn = turn_before;
    let (pointer, m, space, (factor, scroll)) =
        ui.input(|i| (i.pointer.clone(), i.modifiers, i.key_down(egui::Key::Space), wheel(i, wheel_zooms, line, rect.height(), &mut turn)));
    if turn != turn_before {
        ui.data_mut(|d| d.insert_temp(turn_id, turn));
    }
    if turn.glide != 0.0 {
        ui.ctx().request_repaint();
    }
    let hover = pointer.hover_pos().filter(|p| rect.contains(*p));
    // Zoom around the pointer, and scroll ([`wheel`]): fingers pinching on a touch screen may also
    // slide. Then turn the view about the pointer for a trackpad rotation ([`trackpad_turn`]). The
    // tool's events below take their document points from the view this moves to.
    if resp.hovered() {
        if (factor - 1.0).abs() > 1e-6
            && let (Some(p), Some(vm)) = (hover, app.view_mut())
        {
            zoom_about(vm, rect, p, vm.zoom * factor);
        }
        if scroll != egui::Vec2::ZERO {
            scroll_view(app, ui, scroll);
        }
        if let Some(p) = hover
            && let Some(deg) = trackpad_turn(ui, app.session.prefs.trackpad_rotate_view)
            && let Some(vm) = app.view_mut()
        {
            rotate_about(vm, rect, p, deg);
        }
    }
    let v = *app.view().unwrap_or(&View::default());
    let xf = Xf::new(rect, &v);
    // A lifted pen or finger takes the pointer away in the frame it lifts (egui's `PointerGone`),
    // so a tap whose press and lift come in one frame has no hover position: it presses (and
    // double-taps) where it was seen last (#491).
    let at = hover.or_else(|| pointer.interact_pos().filter(|p| rect.contains(*p)));
    let view = app.view_info();
    touch_gestures(app, ui, rect, view);
    // Pressed and held still, a pen or a finger opens the context menu (egui's long touch): the
    // tool's press is dropped, as a right-click presses nothing.
    if resp.long_touched() {
        drop_press(app, ui, view);
    }
    let drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id()));
    // A modifier pressed or released over the canvas re-hovers the tool, so what it changes shows
    // without moving the mouse (Alt switches the Shape Builder to erase mode).
    let mods_changed = ui.data_mut(|d| {
        let id = egui::Id::unique("canvas-mods");
        let prev = d.get_temp::<egui::Modifiers>(id);
        d.insert_temp(id, m);
        prev.is_some_and(|prev| prev != m)
    });
    // Where the tool hovers: the pointer over the canvas itself, not over a floating panel, a menu,
    // a popup or a dialog drawn on top of it ([`on_canvas`]).
    let hover_tool = hover.filter(|p| on_canvas(ui, resp, *p));
    // Whether the tool hovered last frame: coming onto the canvas re-hovers it, even after a single
    // jump egui doesn't count as moving, and leaving it ends the tool's hover (below).
    let was_over = ui.data_mut(|d| {
        let id = egui::Id::unique("canvas-hovered");
        let prev = d.get_temp::<bool>(id).unwrap_or(false);
        d.insert_temp(id, hover_tool.is_some());
        prev
    });
    // What a hover means besides where the pointer is on screen: the tool, the canvas, the view
    // (scrolled, zoomed or turned by the wheel, a gesture, keys or a command) and the document and
    // selection as they are (another tab, an edit, an undo). Under a still pointer, a tool switched
    // to is hovered at once, and when the rest changes, a tool that asks for it
    // ([`Session::tool_wants_rehover`]) is hovered again where the pointer is.
    let context = HoverContext {
        tool: app.session.tool_id(),
        canvas: rect,
        center: v.center,
        zoom: v.zoom,
        rotation: v.rotation,
        doc: app.session.active().map(|st| (st.uid, st.revision)),
    };
    let (tool_switched, context_changed) = ui.data_mut(|d| {
        let id = egui::Id::unique("canvas-hover-context");
        let prev = d.get_temp::<HoverContext>(id);
        d.insert_temp(id, context);
        prev.map_or((false, false), |prev| (prev.tool != context.tool, prev != context))
    });

    // In Presentation Mode a click anywhere goes to the next artboard. The tool receives no press,
    // drag or hover, and a drag does not pan the view. A drag that began before the mode was turned
    // on ends as usual.
    if app.ui.screen_mode == 3 && matches!(drag, None | Some(Drag::Present | Drag::Gesture)) {
        let pressed = drag.is_none() && pointer.primary_pressed() && resp.hovered() && at.is_some();
        // `press` is this frame's new press, or else the press held so far. A quick tap can be
        // pressed and released in one frame.
        let press = if pressed { Some(Drag::Present) } else { drag };
        if press.is_some() && !pointer.primary_down() {
            ui.data_mut(|dd| dd.remove::<Drag>(drag_id()));
            // A tap of several fingers leaves `Drag::Gesture` (`touch_gestures`), and its release
            // does not move the view to the next artboard.
            if press == Some(Drag::Present) && resp.clicked() {
                crate::shortcuts::presentation_step(app, true);
            }
        } else if pressed {
            ui.data_mut(|dd| dd.insert_temp(drag_id(), Drag::Present));
        }
        return;
    }

    // A pen turned to its eraser end selects the Eraser, and a pen turned back to its tip selects
    // the tool before it. The switch runs only while the pointer is in this window. It runs before
    // this frame's press, which then uses the new tool; while a press is held, it waits for the
    // release. A turn made over another window takes effect when the pointer comes in.
    if drag.is_none() && ui.input(|i| i.pointer.has_pointer()) {
        crate::stylus::sync_eraser_tool(app);
    }
    let tool = app.session.tool_id();
    // Held, Space is the Hand tool for the moment, Space+Shift is the Rotate View tool and
    // Cmd+Space is the Zoom tool (with Alt, zooming out), whatever the tool.
    let zoom_mode = if space { m.command } else { tool == "zoom" };
    let rotate_mode = if space { m.shift && !m.command } else { tool == "rotateView" };
    let pan_mode = if space { !m.command && !m.shift } else { tool == "hand" };
    let middle_pan = matches!(drag, Some(Drag::Pan { middle: true, .. }));
    // egui counts a press a few pixels outside the canvas as on it (its interaction radius): only a
    // press on the canvas itself reaches the tools, else it would land at the canvas's centre.
    if pointer.primary_pressed()
        && resp.hovered()
        && !middle_pan
        && let Some(p) = at
    {
        ui.ctx().memory_mut(|mem| mem.stop_text_input());
        app.ui.flyout = None;
        // The artboard's name sits above its corner. A press there renames on a double-click
        // and does not start a tool gesture (a fitted artboard, a new one, Artboard Options).
        if let Some(i) = artboard_name_at(app, ui, &xf, p) {
            ui.data_mut(|dd| dd.insert_temp(name_press_id(), i));
        } else {
            let d = if crate::touch::finger_navigates_only() {
                // A finger (or a palm) once a pen has been seen: it navigates, it doesn't draw.
                Drag::Gesture
            } else if zoom_mode {
                if animated_zoom(&app.session.prefs) { Drag::ZoomScrub { start: p, held: 0.0, moved: false } } else { Drag::ZoomBox { start: p } }
            } else if pan_mode {
                Drag::Pan { start: p, center: v.center, middle: false }
            } else if rotate_mode {
                let c = rect.center();
                Drag::RotateView { start_angle: (p.y - c.y).atan2(p.x - c.x) as f64, start_rot: v.rotation, snaps: !space }
            } else {
                // Cmd with another tool drags with the selection tool used last (the session lends it).
                let ev = PointerEvent { kind: PointerKind::Down, pos: xf.to_doc(p), mods: mods(m, space), pressure: pen_pressure(ui, app, true) };
                dispatch(app, &ev, view);
                crate::document_drag::begin(app, ui.ctx(), ev.pos);
                Drag::Tool
            };
            // Panning, zooming or turning the view takes the pointer from the tool's hover.
            if !tool_holds(d) {
                app.session.tool_hover_ended(view);
            }
            ui.data_mut(|dd| {
                dd.insert_temp(drag_id(), d);
                dd.insert_temp(drag_pos_id(), p);
                dd.insert_temp(drag_layer_id(), resp.layer_id);
                // A new press has not dragged yet, whatever an interrupted one left behind.
                dd.remove::<Point>(tool_drag_doc_id());
                dd.insert_temp(tool_press_doc_id(), xf.to_doc(p));
            });
        }
    } else if drag.is_none()
        && resp.hovered()
        && pointer.button_pressed(egui::PointerButton::Middle)
        && let Some(start) = hover
    {
        // Middle-button drag pans the view whatever the tool.
        ui.data_mut(|dd| dd.insert_temp(drag_id(), Drag::Pan { start, center: v.center, middle: true }));
        app.session.tool_hover_ended(view);
    } else if let Some(d) = drag {
        // The pointer gone (a pen lifted with its press), the drag ends where it was last seen.
        let p = pointer.interact_pos().or_else(|| ui.data(|dd| dd.get_temp(drag_pos_id()))).unwrap_or(rect.center());
        let held = if middle_pan { pointer.button_down(egui::PointerButton::Middle) } else { pointer.primary_down() };
        if held {
            ui.data_mut(|dd| dd.insert_temp(drag_pos_id(), p));
            match d {
                Drag::Pan { start, center, .. } => {
                    let ppp = ui.ctx().pixels_per_point();
                    if let Some(vm) = app.view_mut() {
                        vm.center = center;
                        pan_pixels(vm, ppp, whole_pixels(p - start, ppp));
                    }
                }
                Drag::RotateView { start_angle, start_rot, snaps } => {
                    let c = rect.center();
                    let a = (p.y - c.y).atan2(p.x - c.x) as f64;
                    let mut deg = start_rot + (a - start_angle).to_degrees();
                    if snaps && m.shift {
                        deg = (deg / 15.0).round() * 15.0;
                    }
                    if let Some(vm) = app.view_mut() {
                        vm.rotation = vectorcraft_geom::normalize_deg(deg);
                    }
                }
                Drag::ZoomBox { start } => marquee(ui.painter(), egui::Rect::from_two_pos(start, p)),
                Drag::ZoomScrub { start, held, moved } => {
                    let dt = ui.input(|i| i.unstable_dt).min(0.25);
                    let (held, moved) = (held + dt, moved || (p - start).length() > SCRUB_SLOP);
                    // Dragged sideways it zooms with the pointer, held still it zooms on (Alt: out).
                    let exponent = if moved {
                        f64::from(pointer.delta().x) * SCRUB_ZOOM
                    } else if held > HOLD_DELAY {
                        f64::from(dt) * HOLD_ZOOM * if m.alt { -1.0 } else { 1.0 }
                    } else {
                        0.0
                    };
                    if exponent != 0.0
                        && let Some(vm) = app.view_mut()
                    {
                        zoom_about(vm, rect, start, vm.zoom * exponent.exp());
                    }
                    ui.data_mut(|dd| dd.insert_temp(drag_id(), Drag::ZoomScrub { start, held, moved }));
                }
                // Without a pointer it is over a floating panel's own OS window (`drag_art_out`).
                Drag::Art if pointer.has_pointer() && on_canvas(ui, resp, p) => {
                    // Back on the canvas: the move goes on from where it left, with the pointer.
                    egui::DragAndDrop::clear_payload(ui.ctx());
                    let ev =
                        PointerEvent { kind: PointerKind::Drag, pos: xf.to_doc(p), mods: mods(m, space), pressure: pen_pressure(ui, app, false) };
                    dispatch(app, &ev, view);
                    ui.data_mut(|dd| {
                        dd.insert_temp(drag_id(), Drag::Tool);
                        dd.insert_temp(drag_pos_id(), p);
                    });
                }
                Drag::Art | Drag::Gesture | Drag::Present => {}
                Drag::Tool if drag_art_out(app, ui, resp, p) => {
                    ui.data_mut(|dd| dd.insert_temp(drag_id(), Drag::Art));
                }
                Drag::Tool => {
                    // Every position the pen or mouse passed this frame, not just the last, for a
                    // tool that builds a stroke from them: a fast stroke keeps its curve (a pen or a
                    // mouse reports far more often than frames come). Touch events (a pen on Windows
                    // or the web, a finger) and a macOS pen's moves carry their own pressure; a
                    // mouse's, a pen's on X11 among them, take the host's latest. Other tools get
                    // the last position only, so a costly drag preview runs once a frame.
                    let moves = tool_frame_moves(ui, app);
                    // A frame whose pointer ends where the last one did still moved in between.
                    if tool_drag_moved(ui, &xf, p, &pointer) || !moves.is_empty() {
                        for (at, force) in moves {
                            let pressure = force.unwrap_or_else(|| pen_pressure(ui, app, false));
                            dispatch(app, &PointerEvent { kind: PointerKind::Drag, pos: xf.to_doc(at), mods: mods(m, space), pressure }, view);
                        }
                        let ev =
                            PointerEvent { kind: PointerKind::Drag, pos: xf.to_doc(p), mods: mods(m, space), pressure: pen_pressure(ui, app, false) };
                        dispatch(app, &ev, view);
                    }
                    // Time held (Twirl, Pucker and Bloat keep applying); a stalled frame counts
                    // a quarter second at most.
                    if app.session.tool_wants_ticks() {
                        let dt = f64::from(ui.input(|i| i.unstable_dt).min(0.25));
                        let r = app.session.tool_tick(dt, view);
                        apply_requests(app, r);
                    }
                }
            }
        } else {
            // The last position the tool's drag got: none if the press never dragged.
            let last_drag: Option<Point> = ui.data(|dd| dd.get_temp(tool_drag_doc_id()));
            ui.data_mut(|dd| {
                dd.remove::<Drag>(drag_id());
                dd.remove::<Pos2>(drag_pos_id());
                dd.remove::<Point>(tool_drag_doc_id());
                dd.remove::<Point>(tool_press_doc_id());
            });
            match d {
                Drag::ZoomBox { start } => {
                    let r = egui::Rect::from_two_pos(start, p);
                    if let Some(vm) = app.view_mut() {
                        if r.width() > 8.0 && r.height() > 8.0 {
                            // The marquee's center and its size on screen set the view, at any rotation.
                            vm.center = xf.to_doc(r.center());
                            let fill = (rect.width() / r.width()).min(rect.height() / r.height());
                            vm.zoom = (xf.zoom * f64::from(fill)).clamp(0.0313, 640.0);
                        } else {
                            zoom_about(vm, rect, p, crate::state::next_zoom(vm.zoom, !m.alt));
                        }
                    }
                }
                // A click (not held, not dragged) steps the zoom as with the marquee.
                Drag::ZoomScrub { held, moved: false, .. } if held <= HOLD_DELAY => {
                    if let Some(vm) = app.view_mut() {
                        zoom_about(vm, rect, p, crate::state::next_zoom(vm.zoom, !m.alt));
                    }
                }
                Drag::ZoomScrub { .. } => {}
                Drag::Tool => {
                    // The second click of a double-click: don't fit another artboard on this release.
                    // A double-click on an object does not open Artboard Options. Empty artboard
                    // space still does.
                    if resp.double_clicked() && app.session.tool_id() == "artboard" {
                        app.session.set_tool_option("suppressFitClick", &json!(true));
                    }
                    // A move that came in the release's own frame shapes the preview first: the
                    // tools commit the last preview, which should be where the pointer let go
                    // (#834). Only once the press has dragged, so a click's jitter stays a click
                    // (the Pen's corner anchor, no nudge with Direct Selection).
                    if let Some(last) = last_drag
                        && last != xf.to_doc(p)
                    {
                        let ev =
                            PointerEvent { kind: PointerKind::Drag, pos: xf.to_doc(p), mods: mods(m, space), pressure: pen_pressure(ui, app, false) };
                        dispatch(app, &ev, view);
                    }
                    let ev = PointerEvent { kind: PointerKind::Up, pos: xf.to_doc(p), mods: mods(m, space), pressure: pen_pressure(ui, app, false) };
                    dispatch(app, &ev, view);
                }
                Drag::Art if on_canvas(ui, resp, p) => {
                    // Came back to the canvas and released in one frame: finish the move here.
                    egui::DragAndDrop::clear_payload(ui.ctx());
                    let pressure = pen_pressure(ui, app, false);
                    let pos = xf.to_doc(p);
                    dispatch(app, &PointerEvent { kind: PointerKind::Drag, pos, mods: mods(m, space), pressure }, view);
                    dispatch(app, &PointerEvent { kind: PointerKind::Up, pos, mods: mods(m, space), pressure }, view);
                }
                Drag::Art => {
                    // Released over a panel: the move goes back. (The app ends it before the panels
                    // see the release, so a panel can take the art; this is the headless path.)
                    // With no document open there is nothing to roll back; the tool still gets its up.
                    let _ = app.session.cancel_interaction();
                    dispatch(app, &PointerEvent { kind: PointerKind::Up, pos: Point::ZERO, mods: Mods::default(), pressure: 1.0 }, view);
                }
                Drag::Gesture | Drag::Present => {}
                Drag::Pan { .. } | Drag::RotateView { .. } => {}
            }
        }
    } else if let Some(p) = hover_tool
        && (pointer.is_moving() || mods_changed || !was_over || tool_switched || (context_changed && app.session.tool_wants_rehover()))
    {
        let ev = PointerEvent { kind: PointerKind::Move, pos: xf.to_doc(p), mods: mods(m, space), pressure: 1.0 };
        dispatch(app, &ev, view);
    }
    // The pointer left the canvas (for a panel, or out of the window) or went under a floating
    // panel, menu, popup or dialog, with no press the tool holds: the tool's hover ends, which no
    // hover event would clear (Alt's Distance Guides).
    if was_over && hover_tool.is_none() && !drag.is_some_and(tool_holds) {
        app.session.tool_hover_ended(view);
        ui.ctx().request_repaint();
    }
    // The tool works on another thread (a large selection's planar map): it checks each frame
    // whether the work is done, and then shows the highlight under the pointer.
    if app.session.tool_waiting() {
        if let Err(e) = app.session.tool_poll(view, false) {
            app.status(e.to_string());
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    if pointer.primary_released()
        && let Some(i) = ui.data_mut(|d| d.remove_temp::<usize>(name_press_id()))
    {
        select_named_artboard(app, i);
    }
    if resp.double_clicked()
        && let Some(p) = at
    {
        if let Some(i) = artboard_name_at(app, ui, &xf, p) {
            begin_artboard_name_edit(ui, app, i);
        } else {
            let ev = PointerEvent { kind: PointerKind::DoubleClick, pos: xf.to_doc(p), mods: mods(m, space), pressure: 1.0 };
            let before = app.session.tool_id().to_string();
            dispatch(app, &ev, view);
            // A double-click on type switched a selection tool to the Type tool: the caret goes where
            // the type was clicked, so typing edits it at once.
            if before != "type" && app.session.tool_id() == "type" {
                app.session.set_tool_option("prevTool", &json!(before));
                for kind in [PointerKind::Down, PointerKind::Up] {
                    dispatch(app, &PointerEvent { kind, ..ev }, view);
                }
            }
        }
    }
    if drag.is_some() || pointer.is_moving() || mods_changed {
        ui.ctx().request_repaint();
    }
}

/// How much one point of wheel motion zooms (as `exp(points × WHEEL_ZOOM)`).
const WHEEL_ZOOM: f64 = 0.01;
/// How fast a wheel notch's zoom glides in with Zoom with Mouse Wheel: the time constant (s) of
/// the part still to come.
const WHEEL_GLIDE: f64 = 0.05;

/// What the wheel did across frames ([`wheel`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct WheelTurn {
    /// The last wheel turn was an Alt-wheel one.
    alt: bool,
    /// The zoom (natural log) wheel notches still have to glide in.
    glide: f64,
}
/// Animated Zoom: how much one point of sideways drag zooms (100 points to the right double it).
const SCRUB_ZOOM: f64 = std::f64::consts::LN_2 / 100.0;
/// Animated Zoom: how far the pointer moves before a press is a drag, in points.
const SCRUB_SLOP: f32 = 3.0;
/// Animated Zoom: how long a press is held still before it zooms on, in seconds…
const HOLD_DELAY: f32 = 0.3;
/// …and how fast it zooms then (as `exp(seconds × HOLD_ZOOM)`: twice as close each second).
const HOLD_ZOOM: f64 = std::f64::consts::LN_2;

/// Performance › Animated Zoom, which needs GPU Performance as in Illustrator: the Zoom tool zooms
/// as it is dragged sideways or held, instead of zooming to the area dragged across.
pub(crate) fn animated_zoom(p: &vectorcraft_engine::Prefs) -> bool {
    p.animated_zoom && p.gpu_performance
}

/// Zoom for View → Actual Size (`view.actualSize`). With Preferences › General › Display Print
/// Size at 100% Zoom off, one document point is one screen point. On, one document inch (72 pt)
/// fills an inch of the screen as the system counts it: 96 screen points, the reference density
/// that display scaling is set against (a CSS inch), whatever the scale factor.
pub(crate) fn actual_size_zoom(display_print_size: bool) -> f64 {
    if display_print_size { 96.0 / 72.0 } else { 1.0 }
}

/// Zoom view `vm` of the canvas `rect` to `zoom` (clamped to the zoom range), keeping the document
/// point under screen point `p` where it is.
fn zoom_about(vm: &mut View, rect: egui::Rect, p: Pos2, zoom: f64) {
    let before = Xf::new(rect, vm).to_doc(p);
    vm.zoom = zoom.clamp(0.0313, 640.0);
    vm.center += before - Xf::new(rect, vm).to_doc(p);
}

/// Turn view `vm` of the canvas `rect` by `degrees` clockwise, keeping the document point under
/// screen point `p` where it is.
fn rotate_about(vm: &mut View, rect: egui::Rect, p: Pos2, degrees: f64) {
    let before = Xf::new(rect, vm).to_doc(p);
    vm.rotation = vectorcraft_geom::normalize_deg(vm.rotation + degrees);
    vm.center += before - Xf::new(rect, vm).to_doc(p);
}

/// Move view `vm`'s content by `(dx, dy)` device pixels along the screen's axes, at `ppp` device
/// pixels a point: the canvas's frame moves by exactly those pixels.
fn pan_pixels(vm: &mut View, ppp: f32, (dx, dy): (f64, f64)) {
    let (sn, cs) = vm.rotation.to_radians().sin_cos();
    let k = vm.zoom * f64::from(ppp);
    vm.center -= vectorcraft_geom::Vec2::new((dx * cs + dy * sn) / k, (-dx * sn + dy * cs) / k);
}

/// Screen points `d` in the nearest whole device pixels, at `ppp` device pixels a point.
fn whole_pixels(d: egui::Vec2, ppp: f32) -> (f64, f64) {
    ((f64::from(d.x) * f64::from(ppp)).round(), (f64::from(d.y) * f64::from(ppp)).round())
}

/// What the scrolls of the view have left to move ([`scroll_view`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ScrollRest {
    /// Device pixels along the screen's axes, at most half a pixel each.
    x: f64,
    y: f64,
    /// The document, the view's center, zoom and rotation, and the device pixels a point that the
    /// last scroll left.
    after: Option<(u64, Point, f64, f64, f32)>,
}

/// Scroll the active view so that its content moves `by` screen points (the wheel, a trackpad, a
/// touch screen, a scroll bar), in whole device pixels: the part of a pixel left over is added to
/// the next scroll. It is dropped when the view changed in between (a zoom, a turn, Fit, a pan
/// of another kind, another document) or the device pixels a point did.
fn scroll_view(app: &mut VectorcraftApp, ui: &Ui, by: egui::Vec2) {
    let ppp = ui.ctx().pixels_per_point();
    let Some(doc) = app.session.active().map(|st| st.uid) else { return };
    let Some(vm) = app.view_mut() else { return };
    let id = egui::Id::unique("canvas-scroll-rest");
    let rest = scroll_by(vm, doc, ppp, by, ui.data(|d| d.get_temp(id)).unwrap_or_default());
    ui.data_mut(|d| d.insert_temp(id, rest));
}

/// [`scroll_view`] for view `vm` of document `doc` at `ppp` device pixels a point, after the
/// scrolls that left `rest`: what this scroll leaves.
fn scroll_by(vm: &mut View, doc: u64, ppp: f32, by: egui::Vec2, rest: ScrollRest) -> ScrollRest {
    let (x, y) = if rest.after == Some((doc, vm.center, vm.zoom, vm.rotation, ppp)) { (rest.x, rest.y) } else { (0.0, 0.0) };
    let (x, y) = (x + f64::from(by.x) * f64::from(ppp), y + f64::from(by.y) * f64::from(ppp));
    let (dx, dy) = (x.round(), y.round());
    pan_pixels(vm, ppp, (dx, dy));
    ScrollRest { x: x - dx, y: y - dy, after: Some((doc, vm.center, vm.zoom, vm.rotation, ppp)) }
}

/// How far a trackpad rotation turns before it turns the view, in degrees.
const ROTATE_DEAD_ZONE: f64 = 5.0;
/// How long after its last rotation event a trackpad rotation ends, in seconds.
const ROTATE_GESTURE_END: f64 = 0.25;

/// A trackpad rotation under way ([`trackpad_turn`]).
#[derive(Clone, Copy, Debug)]
struct TrackpadTurn {
    /// Degrees it has turned so far, clockwise.
    total: f64,
    /// When its last rotation event came (egui's input time, in seconds).
    last: f64,
    /// It has reached [`ROTATE_DEAD_ZONE`] and turns the view.
    turning: bool,
}

/// General › Trackpad Gesture to Rotate View (`on`): the degrees, clockwise, that a two-finger
/// rotation on a Mac trackpad turns the view this frame. egui sends the rotation as
/// [`egui::Event::Rotate`], from winit on macOS and from Safari's gesture events in the web build.
/// A rotation does not turn the view until it reaches [`ROTATE_DEAD_ZONE`]; it then turns the view
/// by all of it, and by each later event as it comes. It ends [`ROTATE_GESTURE_END`] after its last
/// event. Rotation events that come while fingers are on a touch screen do not turn the view.
fn trackpad_turn(ui: &Ui, on: bool) -> Option<f64> {
    if !on {
        return None;
    }
    let (deg, now) = ui.input(|i| {
        let rad: f32 =
            if i.multi_touch().is_some() { 0.0 } else { i.events.iter().map(|e| if let egui::Event::Rotate(r) = e { *r } else { 0.0 }).sum() };
        (f64::from(rad).to_degrees(), i.time)
    });
    if deg == 0.0 || !deg.is_finite() {
        return None;
    }
    let id = egui::Id::unique("canvas-trackpad-turn");
    let kept: Option<TrackpadTurn> = ui.data(|d| d.get_temp(id));
    let mut t = kept.filter(|t| now - t.last <= ROTATE_GESTURE_END).unwrap_or(TrackpadTurn { total: 0.0, last: now, turning: false });
    t.total += deg;
    t.last = now;
    let turn = if t.turning {
        Some(deg)
    } else if t.total.abs() >= ROTATE_DEAD_ZONE {
        t.turning = true;
        Some(t.total)
    } else {
        None
    };
    ui.data_mut(|d| d.insert_temp(id, t));
    turn
}

/// One scroll bar along its axis, in screen points: the span it scrolls over (`lo..hi`) and the
/// part of it the canvas shows (`v0..v1`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    lo: f32,
    hi: f32,
    v0: f32,
    v1: f32,
}

impl Span {
    /// The thumb on a track `len` long: where it starts and how long it is.
    fn thumb(self, len: f32) -> (f32, f32) {
        let total = (self.hi - self.lo).max(1.0);
        let size = (len * (self.v1 - self.v0) / total).clamp(MIN_THUMB.min(len), len);
        let room = total - (self.v1 - self.v0);
        let at = if room > 0.0 { (self.v0 - self.lo) / room * (len - size) } else { 0.0 };
        (at.clamp(0.0, (len - size).max(0.0)), size)
    }
    /// Screen points the view moves for each point the thumb moves on a track `len` long.
    fn ratio(self, len: f32) -> f32 {
        let room = (self.hi - self.lo) - (self.v1 - self.v0);
        let free = len - self.thumb(len).1;
        if room > 0.0 && free > 0.0 { room / free } else { 0.0 }
    }
}

const MIN_THUMB: f32 = 20.0;

/// The scroll bars' spans, horizontal then vertical: the art and the artboards (`content`) as the
/// view turns them on screen, half a window past each side, and the view itself, so the thumb is
/// always on its track. The canvas has no edge of its own (no drawing area), so the content sets it.
fn scroll_spans(xf: &Xf, content: Option<Rect>) -> [Span; 2] {
    let r = xf.rect;
    let around = content.map(|c| egui::Rect::from_points(&xf.quad(c)).expand2(r.size() / 2.0)).filter(|b| b.is_finite());
    let b = around.map_or(r, |b| b.union(r));
    [Span { lo: b.left(), hi: b.right(), v0: r.left(), v1: r.right() }, Span { lo: b.top(), hi: b.bottom(), v0: r.top(), v1: r.bottom() }]
}

/// The tracks of the horizontal and the vertical scroll bar, along the bottom and the right edge
/// of the window (`outer`), as long as the canvas (`rect`) beside them.
fn scroll_tracks(rect: egui::Rect, outer: egui::Rect) -> [egui::Rect; 2] {
    let corner = outer.max - vec2(SCROLL_BAR, SCROLL_BAR);
    [
        egui::Rect::from_min_max(pos2(rect.left(), corner.y), pos2(corner.x, outer.bottom())),
        egui::Rect::from_min_max(pos2(corner.x, rect.top()), pos2(outer.right(), corner.y)),
    ]
}

/// The window's scroll bars ([`scroll_spans`]): a thumb dragged scrolls the view along the
/// screen's axis (a turned view too), a click on the track pages towards it, and the wheel over a
/// bar scrolls along it.
fn scroll_bars(app: &mut VectorcraftApp, ui: &Ui, rect: egui::Rect, outer: egui::Rect, t: &Tokens) {
    let content = app.content_bounds();
    let Some(v) = app.view().copied() else { return };
    let xf = Xf::new(rect, &v);
    let painter = ui.painter();
    painter.rect_filled(egui::Rect::from_min_max(outer.max - vec2(SCROLL_BAR, SCROLL_BAR), outer.max), 0.0, t.ruler);
    let hover = ui.input(|i| i.pointer.hover_pos());
    // Screen points the view moves this frame, or the center a dragged thumb's drag began at and
    // the screen points it has moved the view since.
    let mut moved = egui::Vec2::ZERO;
    let mut center = None;
    for (k, (track, span)) in scroll_tracks(rect, outer).into_iter().zip(scroll_spans(&xf, content)).enumerate() {
        let horizontal = k == 0;
        let along = |p: Pos2| if horizontal { p.x - track.left() } else { p.y - track.top() };
        let axis = if horizontal { vec2(1.0, 0.0) } else { vec2(0.0, 1.0) };
        let len = if horizontal { track.width() } else { track.height() };
        let (at, size) = span.thumb(len);
        let on_thumb = |p: Pos2| (at..=at + size).contains(&along(p));
        let id = egui::Id::unique(("canvas-scroll-bar", k));
        let resp = ui.interact(track, id, Sense::click_and_drag());
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
            && on_thumb(p)
        {
            // The span moves with the view: the drag keeps the one it started with.
            let d = ThumbDrag { from: along(p), center: v.center, ratio: span.ratio(len), min: span.lo - span.v0, max: span.hi - span.v1 };
            ui.data_mut(|dd| dd.insert_temp(id, d));
        }
        let drag: Option<ThumbDrag> = ui.data(|dd| dd.get_temp(id));
        if let Some(d) = drag {
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let by = ((along(p) - d.from) * d.ratio).clamp(d.min, d.max);
                center = Some((d.center, axis * by));
            }
            if !resp.dragged() || resp.drag_stopped() {
                ui.data_mut(|dd| dd.remove::<ThumbDrag>(id));
            }
        } else if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            // A page is most of the window, and never past the end of the span.
            let page = 0.9 * (span.v1 - span.v0);
            if along(p) < at {
                moved -= axis * page.min(span.v0 - span.lo);
            } else if along(p) > at + size {
                moved += axis * page.min(span.hi - span.v1);
            }
        }
        if resp.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            moved -= axis * if horizontal { d.x + d.y } else { d.y };
        }
        painter.rect_filled(track, 0.0, t.ruler);
        let edge = if horizontal { [track.left_top(), track.right_top()] } else { [track.left_top(), track.left_bottom()] };
        painter.line_segment(edge, Stroke::new(1.0, t.border));
        let thumb = if horizontal {
            egui::Rect::from_min_size(pos2(track.left() + at, track.top()), vec2(size, track.height())).shrink2(vec2(1.0, 3.0))
        } else {
            egui::Rect::from_min_size(pos2(track.left(), track.top() + at), vec2(track.width(), size)).shrink2(vec2(3.0, 1.0))
        };
        let hot = drag.is_some() || hover.is_some_and(|p| thumb.contains(p));
        painter.rect_filled(thumb, CornerRadius::same(3), if hot { t.text_dim } else { t.text_disabled });
    }
    let ppp = ui.ctx().pixels_per_point();
    if let Some((c, by)) = center
        && let Some(vm) = app.view_mut()
    {
        vm.center = c;
        pan_pixels(vm, ppp, whole_pixels(-by, ppp));
    }
    if moved != egui::Vec2::ZERO {
        scroll_view(app, ui, -moved);
    }
}

/// A scroll bar thumb being dragged: where along the track it was pressed, and the view's centre,
/// the span's travel ratio ([`Span::ratio`]) and how far the view could move either way then.
#[derive(Clone, Copy, Debug)]
struct ThumbDrag {
    from: f32,
    center: Point,
    ratio: f32,
    min: f32,
    max: f32,
}

/// A marquee around screen rectangle `r`: dark dashes over a light line, so it shows on a white
/// artboard and on the grey pasteboard alike (#725).
fn marquee(p: &egui::Painter, r: egui::Rect) {
    marquee_through(p, &[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]);
}

/// A [`marquee`] through the screen corners `q` in turn, back to the first: a document rectangle's
/// [`Xf::quad`] in a rotated view.
fn marquee_through(p: &egui::Painter, q: &[Pos2]) {
    let pts: Vec<Pos2> = q.iter().chain(q.first()).copied().collect();
    p.add(Shape::line(pts.clone(), Stroke::new(1.0, Color32::from_white_alpha(230))));
    for w in pts.windows(2) {
        p.extend(Shape::dashed_line(&[w[0], w[1]], Stroke::new(1.0, Color32::from_gray(40)), 3.0, 3.0));
    }
}

/// What the wheel, a pinch and a two-finger drag on a touch screen did over the canvas this frame:
/// a zoom factor (about the pointer) and a scroll (screen points the content moves). The wheel scrolls, Cmd- and Alt-wheel (Option
/// on the Mac) zoom. With General › Zoom with Mouse Wheel (`wheel_zooms`) the wheel and Alt-wheel
/// zoom, Shift-wheel scrolls up and down and Cmd/Ctrl-wheel sideways. `line` and `page`: points
/// per wheel line and page. `turn`: kept by the caller across frames. A mouse wheel's notches zoom
/// in a glide over a few frames, as scrolling does (#888); a trackpad's fine steps at once.
fn wheel(i: &egui::InputState, wheel_zooms: bool, line: f32, page: f32, turn: &mut WheelTurn) -> (f64, egui::Vec2) {
    // Fingers on a touch screen pan the content with them (a trackpad sends scrolls instead).
    let pan = i.multi_touch().map_or(egui::Vec2::ZERO, |t| t.translation_delta);
    if !wheel_zooms {
        // egui makes Cmd-wheel (and a pinch) its zoom and the rest a scroll it spreads over a few
        // frames: the rest of an Alt-wheel turn zooms too, however soon Alt is let go.
        if let Some(m) = i.events.iter().rev().find_map(|e| if let egui::Event::MouseWheel { modifiers, .. } = e { Some(modifiers) } else { None }) {
            turn.alt = m.alt && !m.command;
        }
        let (zoom, scroll) = (f64::from(i.zoom_delta()), i.smooth_scroll_delta);
        return if turn.alt && scroll != egui::Vec2::ZERO {
            (zoom * (f64::from(scroll.x + scroll.y) * WHEEL_ZOOM).exp(), pan)
        } else {
            (zoom, scroll + pan)
        };
    }
    // The wheel events themselves: egui's own handling turns Cmd-wheel into a zoom.
    let mut zoom = i.multi_touch().map_or(1.0, |t| f64::from(t.zoom_delta));
    let mut scroll = pan;
    for e in &i.events {
        match e {
            egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                let d = match unit {
                    egui::MouseWheelUnit::Point => *delta,
                    egui::MouseWheelUnit::Line => *delta * line,
                    egui::MouseWheelUnit::Page => *delta * page,
                };
                if modifiers.command {
                    scroll.x += d.x + d.y;
                } else if modifiers.shift {
                    scroll.y += d.x + d.y;
                } else if *unit == egui::MouseWheelUnit::Point {
                    zoom *= (f64::from(d.y) * WHEEL_ZOOM).exp();
                    scroll.x += d.x;
                } else {
                    turn.glide += f64::from(d.y) * WHEEL_ZOOM;
                    scroll.x += d.x;
                }
            }
            egui::Event::Zoom(f) => zoom *= f64::from(*f),
            _ => {}
        }
    }
    // This frame's part of the glide: what is left shrinks by e every WHEEL_GLIDE seconds.
    let part = if turn.glide.abs() < 1e-3 { 1.0 } else { 1.0 - (-f64::from(i.stable_dt.min(0.1)) / WHEEL_GLIDE).exp() };
    let step = turn.glide * part;
    turn.glide -= step;
    (zoom * step.exp(), scroll)
}

/// Enable Touch Gestures: this frame's finger contacts on the canvas ([`crate::touch`]). A second
/// finger drops the press the first one began (its tool interaction rolled back; the rest of the
/// gesture presses nothing), and a tap of two fingers undoes, one of three redoes.
fn touch_gestures(app: &mut VectorcraftApp, ui: &Ui, rect: egui::Rect, view: vectorcraft_engine::ViewInfo) {
    if !app.session.prefs.touch_gestures {
        return;
    }
    let (touches, now): (Vec<_>, f64) = ui.input(|i| {
        let t = i.events.iter().filter_map(|e| match e {
            egui::Event::Touch { id, phase, pos, .. } => Some((*id, *phase, *pos)),
            _ => None,
        });
        (t.collect(), i.time)
    });
    if touches.is_empty() {
        return;
    }
    let key = egui::Id::unique("canvas-touch");
    let mut taps: crate::touch::Taps = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    let mut tap = None;
    for (id, phase, pos) in touches {
        // A gesture begins on the canvas.
        if phase == egui::TouchPhase::Start && !taps.is_down() && !rect.contains(pos) {
            continue;
        }
        // A cancelled contact (a palm the shell drops when a pen lands) keeps nothing it began.
        if phase == egui::TouchPhase::Cancel {
            drop_press(app, ui, view);
        }
        match taps.feed(id, phase, pos, now) {
            Some(crate::touch::Touch::Fingers) => {
                drop_press(app, ui, view);
                ui.data_mut(|d| d.insert_temp(drag_id(), Drag::Gesture));
            }
            Some(crate::touch::Touch::Tap(n)) => tap = crate::touch::tap_command(n),
            None => {}
        }
    }
    ui.data_mut(|d| d.insert_temp(key, taps));
    // In Presentation Mode a tap of two or three fingers undoes or redoes nothing.
    if let Some(cmd) = tap
        && app.ui.screen_mode < 3
    {
        crate::menus::invoke(app, cmd, serde_json::json!({}));
    }
}

/// Drop the tool press in progress, if any: nothing it began is kept, and the rest of the press
/// does nothing ([`Drag::Gesture`]).
fn drop_press(app: &mut VectorcraftApp, ui: &Ui, view: vectorcraft_engine::ViewInfo) {
    match ui.data(|d| d.get_temp::<Drag>(drag_id())) {
        Some(Drag::Tool | Drag::Art) => {
            // The tool's mouse-up returns it to rest. A move waiting over a panel is still live
            // and would otherwise outlive the drag that owns it (#2826).
            // Nothing to roll back without a document; the tool still gets its mouse-up.
            let _ = app.session.cancel_interaction();
            dispatch(app, &PointerEvent { kind: PointerKind::Up, pos: Point::ZERO, mods: Mods::default(), pressure: 1.0 }, view);
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
        Some(_) => {}
        None => return,
    }
    ui.data_mut(|d| d.insert_temp(drag_id(), Drag::Gesture));
}

/// A Selection tool move dragged off the canvas (to `p`, over a panel) becomes a panel drag of
/// the selected art ([`widgets::PanelDrag::Art`]): the art waits where it last moved on the
/// canvas, and a panel it is released on takes it (the Graphic Styles panel makes a style of it).
/// Back on the canvas, [`Drag::Art`] takes the move up again where the pointer is (#2826).
fn drag_art_out(app: &VectorcraftApp, ui: &Ui, resp: &egui::Response, p: Pos2) -> bool {
    let Some(st) = app.session.active().filter(|_| app.session.tool_id() == "selection") else { return false };
    // The Selection tool's interaction while it moves (or Alt-copies) the selection.
    if !st.interaction.as_ref().is_some_and(|i| i.label == "Move" || i.label == "Copy") {
        return false;
    }
    // A floating panel in an OS window of its own takes the art too: craft-ui hands that window
    // the pointer while it is over it, and this window sees the pointer go.
    craft_ui::docking::route_app_drag(ui.ctx(), crate::panel_docking::area_id());
    if on_canvas(ui, resp, p) && ui.input(|i| i.pointer.has_pointer()) {
        return false;
    }
    let ids = st.selection.objects.clone();
    egui::DragAndDrop::set_payload(ui.ctx(), widgets::PanelDrag::Art(ids));
    true
}

/// A Selection tool move released over a panel: end it before the panels see the release, so a
/// panel that takes the art ([`widgets::PanelDrag::Art`]) takes it where it was, and no move is
/// left open (#2826). The art stays where it last moved while the pointer is over the panel.
pub(crate) fn release_over_panel(app: &mut VectorcraftApp, ctx: &egui::Context) {
    crate::document_drag::cleanup(app, ctx);
    let art = matches!(ctx.data(|d| d.get_temp::<Drag>(drag_id())), Some(Drag::Art));
    // Over a floating panel's own OS window the pointer is gone from this window, and the release
    // comes here later, at a point of this window that may lie on the canvas: that panel takes it.
    let gone_id = drag_id().with("art-gone");
    if !ctx.input(|i| i.pointer.primary_released()) {
        if art {
            let gone = ctx.input(|i| !i.pointer.has_pointer());
            ctx.data_mut(|d| d.insert_temp(gone_id, gone));
        }
        return;
    }
    let gone = ctx.data_mut(|d| d.remove_temp::<bool>(gone_id)).unwrap_or(false);
    let p = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
    if !art {
        return;
    }
    let layer = ctx.data(|d| d.get_temp::<egui::LayerId>(drag_layer_id()));
    let on = !gone && p.is_some_and(|p| app.canvas_rect.is_some_and(|r| r.contains(p)) && ctx.layer_id_at(p).is_none_or(|l| Some(l) == layer));
    if on {
        // The canvas's release finishes the move.
        return;
    }
    end_art_move(app, ctx);
}

/// A release in a floating panel's own OS window, during an art drag from the canvas: the move
/// ends before that panel takes the art. The main window would only see it on its next pass.
pub(crate) fn release_art_in_window(app: &mut VectorcraftApp, ctx: &egui::Context) {
    let art = matches!(ctx.data(|d| d.get_temp::<Drag>(drag_id())), Some(Drag::Art));
    if art && ctx.input(|i| i.pointer.primary_released()) {
        end_art_move(app, ctx);
    }
}

/// Roll back the art move dragged off the canvas; a panel takes the art instead.
fn end_art_move(app: &mut VectorcraftApp, ctx: &egui::Context) {
    // With no document open there is nothing to roll back; the tool still gets its mouse-up.
    let _ = app.session.cancel_interaction();
    let view = app.view_info();
    dispatch(app, &PointerEvent { kind: PointerKind::Up, pos: Point::ZERO, mods: Mods::default(), pressure: 1.0 }, view);
    ctx.data_mut(|d| {
        d.remove::<Drag>(drag_id());
        d.remove::<bool>(drag_id().with("art-gone"));
    });
}

/// Send a pointer event to the active tool and act on UI requests (dialogs, tool switches).
pub fn dispatch(app: &mut VectorcraftApp, ev: &PointerEvent, view: vectorcraft_engine::ViewInfo) {
    let r = app.session.pointer(ev, view);
    // The artboard the Artboard tool makes active is the active one (the navigator's, the
    // Artboards panel's).
    if app.session.tool_id() == "artboard"
        && let Some(i) = app.session.tool_options()["active"].as_u64().and_then(|i| usize::try_from(i).ok())
        && let Some(v) = app.view_mut()
    {
        v.artboard = i;
    }
    // A press with a selection tool inside an artboard makes it the active one (#693), as the
    // Artboard tool's does: Paste in Place and in Front or Back then paste onto it, Align to
    // Artboard aligns to it, and All on Active Artboard selects on it.
    if ev.kind == PointerKind::Down
        && vectorcraft_tools::catalog::is_selection_tool(app.session.tool_id())
        && let Some(i) = app.session.active().and_then(|d| d.doc.artboard_at(ev.pos))
        && let Some(v) = app.view_mut()
    {
        v.artboard = i;
    }
    apply_requests(app, r);
}

/// Act on what a tool event asked the UI for (dialogs, tool switches), or show its error.
pub fn apply_requests(app: &mut VectorcraftApp, r: vectorcraft_engine::Result<Vec<vectorcraft_engine::UiRequest>>) {
    match r {
        Ok(reqs) => {
            for r in reqs {
                match r {
                    vectorcraft_engine::UiRequest::Dialog(kind, p) => crate::dialogs::open_tool_dialog(app, &kind, p),
                    vectorcraft_engine::UiRequest::SwitchTool(t) => app.select_tool(&t),
                    vectorcraft_engine::UiRequest::Status(msg) => app.status(msg),
                    vectorcraft_engine::UiRequest::Place(p) => crate::place::place_from_cursor(app, &p),
                }
            }
        }
        Err(e) => app.status(e.to_string()),
    }
}

/// The transparency grid's 2 × 2-cell tile in the document's colours (Document Setup), cached per
/// look, and its cell size in screen points.
pub(crate) fn checker_texture(ctx: &egui::Context, setup: &vectorcraft_doc::DocSetup) -> (egui::TextureHandle, f32) {
    let colors = setup.grid_colors.map(|c| crate::panels::c32(&c));
    let id = egui::Id::unique("transparency-grid-tile");
    let cached = ctx.data(|d| d.get_temp::<([Color32; 2], egui::TextureHandle)>(id)).filter(|(c, _)| *c == colors);
    let tex = cached.map(|(_, t)| t).unwrap_or_else(|| {
        let [a, b] = colors;
        let img = egui::ColorImage::new([2, 2], vec![a, b, b, a]);
        let opts = egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::NEAREST };
        let t = ctx.load_texture(TRANSPARENCY_GRID, img, opts);
        ctx.data_mut(|d| d.insert_temp(id, (colors, t.clone())));
        t
    });
    (tex, setup.grid_size.cell())
}

/// Name of the transparency grid's texture.
pub(crate) const TRANSPARENCY_GRID: &str = "transparency grid";

/// The transparency grid over the artboard `ab` (screen corners `quad`): one textured quad whose
/// cells stay `cell` screen points at every zoom and turn with a rotated view.
fn checker(p: &egui::Painter, quad: &[Pos2], ab: Rect, zoom: f64, tex: egui::TextureId, cell: f32) {
    // Two cells per texture repeat.
    let per = (2.0 * cell as f64 / zoom).max(1e-9);
    textured_quad(p, tex, quad, ((ab.width() / per) as f32, (ab.height() / per) as f32));
}

/// One quad drawn with `tex`: its corners `quad` (from the top left, clockwise) show the texture
/// from (0, 0) to (`u`, `v`).
fn textured_quad(p: &egui::Painter, tex: egui::TextureId, quad: &[Pos2], (u, v): (f32, f32)) {
    let mut mesh = egui::Mesh::with_texture(tex);
    for (pos, uv) in quad.iter().zip([pos2(0.0, 0.0), pos2(u, 0.0), pos2(u, v), pos2(0.0, v)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos: *pos, uv, color: Color32::WHITE });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    p.add(Shape::mesh(mesh));
}

/// The zoom the pixel grid shows from in Pixel Preview (600%), and its colour: a translucent grey,
/// so the art's pixels read through it.
const PIXEL_GRID_ZOOM: f64 = 6.0;
const PIXEL_GRID: Color32 = Color32::from_rgba_premultiplied(64, 64, 64, 96);

/// The look of the grid or of the guides (Preferences › Guides & Grid › Color and Style).
#[derive(Clone, Copy)]
struct LineLook {
    color: Color32,
    dots: bool,
}

impl LineLook {
    fn grid(prefs: &vectorcraft_engine::Prefs) -> Self {
        Self { color: pref_color(&prefs.grid_color, Color32::from_gray(200)), dots: prefs.grid_style == "dots" }
    }

    fn guides(prefs: &vectorcraft_engine::Prefs, t: &Tokens) -> Self {
        Self { color: pref_color(&prefs.guide_color, t.guide), dots: prefs.guide_style == "dots" }
    }

    /// A guide from `a` to `b`: a line, or dots.
    fn guide(self, p: &egui::Painter, a: Pos2, b: Pos2) {
        if self.dots {
            p.extend(Shape::dotted_line(&[a, b], self.color, 4.0, 0.75));
        } else {
            p.line_segment([a, b], Stroke::new(1.0, self.color));
        }
    }
}

/// A `#rrggbb` colour preference, `fallback` when it doesn't parse.
fn pref_color(hex: &str, fallback: Color32) -> Color32 {
    vectorcraft_color::Color::from_hex(hex).map_or(fallback, |c| crate::panels::c32(&c))
}

/// The view in document space: the box round the canvas's corners (the view may be rotated).
fn view_bounds(xf: &Xf) -> Rect {
    let r = xf.rect;
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|c| xf.to_doc(c));
    corners.iter().fold(Rect::from_points(corners[0], corners[0]), |b, c| b.union_pt(*c))
}

/// The part of the segment from `a` to `b` inside `r`, none when the segment misses it.
fn clip_segment(a: Pos2, b: Pos2, r: egui::Rect) -> Option<(Pos2, Pos2)> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    // Each edge as (the run toward it, the room before it).
    for (run, room) in [(-d.x, a.x - r.left()), (d.x, r.right() - a.x), (-d.y, a.y - r.top()), (d.y, r.bottom() - a.y)] {
        if run == 0.0 {
            if room < 0.0 {
                return None;
            }
        } else if run < 0.0 {
            t0 = t0.max(room / run);
        } else {
            t1 = t1.min(room / run);
        }
    }
    (t0 <= t1).then(|| (a + d * t0, a + d * t1))
}

/// The grid (View → Show Grid): a gridline every `spacing` and `subdiv` lighter lines between
/// (left out when they would crowd). In the Dots style, a dot where gridlines cross the
/// subdivisions instead.
fn grid(p: &egui::Painter, xf: &Xf, spacing: f64, subdiv: u32, look: LineLook) {
    // The painter clips to the canvas.
    let view = view_bounds(xf);
    let sub = spacing / subdiv.max(1) as f64;
    let step = if sub * xf.zoom >= 6.0 { sub } else { spacing };
    // Dots crowd sooner than lines.
    if step * xf.zoom < 4.0 || (look.dots && spacing * xf.zoom < 8.0) {
        return;
    }
    let major = |v: f64| (v / spacing - (v / spacing).round()).abs() < 1e-6;
    // The lines across the view (where along their axis), at least 4 px apart (and capped).
    let lines = |from: f64, to: f64| {
        let first = (from / step).floor();
        (0..10_000u32).map(move |i| (first + f64::from(i)) * step).take_while(move |v| *v <= to)
    };
    if look.dots {
        let mut mesh = egui::Mesh::default();
        for x in lines(view.x0, view.x1) {
            for y in lines(view.y0, view.y1).filter(|y| major(x) || major(*y)) {
                mesh.add_colored_rect(egui::Rect::from_center_size(xf.to_screen(Point::new(x, y)), vec2(1.5, 1.5)), look.color);
            }
        }
        p.add(Shape::mesh(mesh));
        return;
    }
    let minor = look.color.gamma_multiply(0.4);
    let stroke = |major: bool| Stroke::new(1.0, if major { look.color } else { minor });
    for x in lines(view.x0, view.x1) {
        p.line_segment([xf.to_screen(Point::new(x, view.y0)), xf.to_screen(Point::new(x, view.y1))], stroke(major(x)));
    }
    for y in lines(view.y0, view.y1) {
        p.line_segment([xf.to_screen(Point::new(view.x0, y)), xf.to_screen(Point::new(view.x1, y))], stroke(major(y)));
    }
}

/// A ruler label for `v` (in the ruler's unit) on a ruler whose labels are `step` apart: whole
/// numbers, or the fewest decimals that write `step` exactly, so a binary step keeps its value
/// (an inch ruler's 1/16 reads `0.0625`, not the `0.06` that rounding the step's magnitude gave).
/// How far down the left ruler a label of `chars` characters reaches, counting from its top: it
/// is spelled one character per line, [`RULER_CHAR`] apart, and the last one is a line tall.
pub(crate) fn vertical_label_height(chars: usize) -> f32 {
    chars.saturating_sub(1) as f32 * RULER_CHAR + RULER_LINE
}

/// A ruler label's line spacing and line height on the left ruler.
const RULER_CHAR: f32 = 8.5;
const RULER_LINE: f32 = 9.5;

pub(crate) fn ruler_label(v: f64, step: f64) -> String {
    let exact = |d: u32| (step * 10f64.powi(d as i32)).fract().abs() < 1e-9;
    let decimals = (0..=7).find(|d| exact(*d)).unwrap_or(7) as usize;
    let s = format!("{v:.decimals$}");
    // Trailing zeros go, as `Unit::number` drops them: a sixteenth-inch ruler labels the inch `1`,
    // not `1.0000`, which the vertical ruler would spell down six lines.
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { &s };
    if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { "0".into() } else { s.into() }
}

/// The rulers, numbered in `unit` (the General unit).
/// The top ruler, the left ruler and the box where they meet.
fn ruler_rects(full: egui::Rect) -> [egui::Rect; 3] {
    let top = egui::Rect::from_min_max(pos2(full.left() + RULER, full.top()), pos2(full.right(), full.top() + RULER));
    let left = egui::Rect::from_min_max(pos2(full.left(), full.top() + RULER), pos2(full.left() + RULER, full.bottom()));
    [top, left, egui::Rect::from_min_size(full.min, vec2(RULER, RULER))]
}

/// A drag from a ruler onto the canvas makes a guide where the button is released (the engine's
/// `Session::ruler_guide`): a horizontal one from the top ruler, a vertical one from the left
/// ruler, snapped as a moved guide is (with Shift to the ruler's ticks). Released anywhere else,
/// it makes none. The corner drags the rulers' origin or resets it on a double-click.
fn ruler_input(app: &mut VectorcraftApp, ui: &Ui, full: egui::Rect, canvas: egui::Rect, xf: &Xf) -> Option<Point> {
    let [top, left, corner] = ruler_rects(full);
    // Right-click on a ruler or the origin box: the document units, to swap between them as
    // Preferences ▸ Units ▸ General does ([`crate::menus::ruler_menu_body`]).
    let mut unit_clicked = None;
    for (r, vertical, id) in [(top, false, "ruler-top"), (left, true, "ruler-left")] {
        // Click-and-drag so the same widget drags out guides (left) and opens the unit menu (right).
        let resp = ui.interact(r, egui::Id::unique(id), Sense::click_and_drag());
        resp.context_menu(|ui| crate::menus::ruler_menu_body(app, ui, &mut unit_clicked));
        let kind = if resp.drag_stopped() {
            PointerKind::Up
        } else if resp.dragged() {
            PointerKind::Drag
        } else {
            continue;
        };
        let p = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
        let on_canvas = p.is_some_and(|p| canvas.contains(p));
        if on_canvas {
            // The new guide shows even if guides were hidden.
            app.ui.view.guides = true;
        }
        let ev = PointerEvent { kind, pos: p.map_or(Point::ZERO, |p| xf.to_doc(p)), mods: mods(ui.input(|i| i.modifiers), false), pressure: 1.0 };
        if let Err(e) = app.session.ruler_guide(vertical, &ev, on_canvas, app.view_info()) {
            app.status(e.to_string());
        }
    }
    let corner = ui.interact(corner, egui::Id::unique("ruler-corner"), Sense::click_and_drag());
    corner.context_menu(|ui| crate::menus::ruler_menu_body(app, ui, &mut unit_clicked));
    if let Some((id, p)) = unit_clicked {
        crate::menus::invoke(app, &id, p);
    }
    if corner.double_clicked_by(egui::PointerButton::Primary) {
        let artboard = app.view().map_or(0, |v| v.artboard);
        if let Err(e) = app.run("document.resetRulerOrigin", json!({"artboard": artboard})) {
            app.status(e.to_string());
        }
    }
    // Keep previews in the UI: only a release on the canvas edits the document, once. Cancelling
    // never adds an undo step or interferes with the drawing tool's interaction.
    let drag_id = ruler_origin_drag_id();
    let uid = app.session.active().map(|d| d.uid);
    if corner.drag_started_by(egui::PointerButton::Primary) {
        ui.data_mut(|d| d.insert_temp(drag_id, (uid, false)));
    }
    let mut preview = None;
    if let Some((started_in, mut cancelled)) = ui.data(|d| d.get_temp::<(Option<u64>, bool)>(drag_id)) {
        cancelled |= started_in != uid || ui.input(|i| !i.focused || i.key_pressed(egui::Key::Escape));
        let at = corner.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos())).filter(|p| canvas.contains(*p)).map(|p| xf.to_doc(p));
        if corner.drag_stopped_by(egui::PointerButton::Primary) {
            ui.data_mut(|d| d.remove::<(Option<u64>, bool)>(drag_id));
            if !cancelled
                && let Some(at) = at
                && let Err(e) = app.run("document.setRulerOrigin", json!({"x": at.x, "y": at.y, "artboard": app.view().map_or(0, |v| v.artboard)}))
            {
                app.status(e.to_string());
            }
        } else if !ui.input(|i| i.pointer.primary_down()) {
            ui.data_mut(|d| d.remove::<(Option<u64>, bool)>(drag_id));
        } else {
            ui.data_mut(|d| d.insert_temp(drag_id, (started_in, cancelled)));
            if !cancelled {
                preview = at;
            }
        }
    }
    preview
}

/// The key of the ruler origin drag's state in egui's temporary data.
fn ruler_origin_drag_id() -> egui::Id {
    egui::Id::unique("ruler-origin-drag")
}

/// Is a drag from the rulers' corner in progress? It stays true after Escape cancels the move, until
/// the button is released.
pub(crate) fn ruler_origin_dragging(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<(Option<u64>, bool)>(ruler_origin_drag_id()).is_some())
}

/// What the rulers count, and from where: `unit`, from `origin` (document points), with each
/// unit stretched `stretch` times along the document's x and y, which a video ruler's pixel aspect
/// ratio makes differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RulerScale {
    pub(crate) unit: Unit,
    pub(crate) origin: Point,
    pub(crate) stretch: [f64; 2],
}

impl RulerScale {
    /// The rulers in `unit` (the General unit), from `origin` (the ruler origin).
    pub(crate) fn document(unit: Unit, origin: Point) -> Self {
        Self { unit, origin, stretch: [1.0, 1.0] }
    }

    /// View › Rulers › Show Video Rulers on artboard `ab`: pixels from its top-left corner, each
    /// pixel as many points wide as its Video Ruler Pixel Aspect Ratio and one point tall, so the
    /// rulers read the artboard's size in video pixels
    /// (https://helpx.adobe.com/illustrator/desktop/create-manage-artboards/organize-manage-artboards/set-video-display-options-for-artboards.html).
    pub(crate) fn video(ab: &vectorcraft_doc::Artboard) -> Self {
        Self { unit: Unit::Pixels, origin: Point::new(ab.rect.x0, ab.rect.y0), stretch: [ab.video_ruler_pixel_aspect_ratio, 1.0] }
    }

    /// How far document point `p` lies from the origin along `axis` (a unit direction in the
    /// document), in points of this scale: each document coordinate divided by its stretch.
    /// Divided by the unit's points, it is the value the ruler reads there.
    pub(crate) fn along(&self, p: Point, axis: vectorcraft_geom::Vec2) -> f64 {
        let d = p - self.origin;
        d.x / self.stretch[0] * axis.x + d.y / self.stretch[1] * axis.y
    }

    /// The screen points one point of this scale spans along `axis` at `zoom` (screen points per
    /// document point): `zoom` itself when nothing is stretched.
    pub(crate) fn zoom_along(&self, zoom: f64, axis: vectorcraft_geom::Vec2) -> f64 {
        zoom / (axis.x * axis.x / self.stretch[0] + axis.y * axis.y / self.stretch[1])
    }
}

fn rulers(ui: &Ui, full: egui::Rect, xf: &Xf, hover: Option<Point>, scale: RulerScale, t: &Tokens) {
    let p = ui.painter();
    let [top, left, corner] = ruler_rects(full);
    for r in [top, left, corner] {
        p.rect_filled(r, 0.0, t.ruler);
    }
    p.line_segment([top.left_bottom(), top.right_bottom()], Stroke::new(1.0, t.border));
    p.line_segment([left.right_top(), left.right_bottom()], Stroke::new(1.0, t.border));
    // Crosshair in the origin box.
    p.line_segment([corner.center() - vec2(4.0, 0.0), corner.center() + vec2(4.0, 0.0)], Stroke::new(1.0, t.ruler_tick));
    p.line_segment([corner.center() - vec2(0.0, 4.0), corner.center() + vec2(0.0, 4.0)], Stroke::new(1.0, t.ruler_tick));
    let font = egui::FontId::proportional(9.5);
    // Each ruler measures along its own edge, in the canvas's coordinates turned with the view
    // (#826): at 0° the top one reads x and the left one y. `draw` gets each tick's distance
    // (screen points) from `from` along the ruler, its length, and the label of a labelled one.
    let ticks = |from: Pos2, to: Pos2, axis: vectorcraft_geom::Vec2, draw: &mut dyn FnMut(f32, f32, Option<String>)| {
        // Pick a label step (in the unit) that gives ≥ 50 px between labels on this ruler, at its
        // own scale; positions below are in the unit (`per` points of the scale each).
        let zoom = scale.zoom_along(xf.zoom, axis);
        let (per, step) = (scale.unit.points(), scale.unit.ruler_step(zoom));
        let minor = scale.unit.ruler_minor(step);
        let near = |v: f64, every: f64| ((v / every).round() * every - v).abs() < minor * 0.01;
        let (a, b) = (scale.along(xf.to_doc(from), axis), scale.along(xf.to_doc(to), axis));
        let mut v = (a / per / minor).floor() * minor;
        while v * per <= b {
            // A tick's length: the labelled ones longest, the halves between them shorter.
            let (len, label) = if near(v, step) {
                (RULER, Some(ruler_label(v, step)))
            } else if near(v, step / 2.0) {
                (7.0, None)
            } else {
                (4.0, None)
            };
            draw(((v * per - a) * zoom) as f32, len, label);
            v += minor;
        }
    };
    // The canvas's directions along the screen's x and y.
    let (sn, cs) = xf.rot.sin_cos();
    let clip_top = p.with_clip_rect(top);
    ticks(top.left_top(), top.right_top(), vectorcraft_geom::Vec2::new(cs, -sn), &mut |d, len, label| {
        let sx = top.left() + d;
        clip_top.line_segment([pos2(sx, top.bottom() - len), pos2(sx, top.bottom())], Stroke::new(1.0, t.ruler_tick));
        if let Some(text) = label {
            clip_top.text(pos2(sx + 2.0, top.top() + 1.0), egui::Align2::LEFT_TOP, text, font.clone(), t.ruler_tick);
        }
    });
    let clip_left = p.with_clip_rect(left);
    // A label spelled down the ruler is tall, and a long one can reach the next label's place:
    // "-0.0625" is seven characters, 60 pt of ruler, where a sixteenth-inch step leaves 54 pt at
    // zoom 12. The ticks all stay; a label that would run into the one above it is left out.
    let mut filled_to = f32::NEG_INFINITY;
    ticks(left.left_top(), left.left_bottom(), vectorcraft_geom::Vec2::new(sn, cs), &mut |d, len, label| {
        let sy = left.top() + d;
        clip_left.line_segment([pos2(left.right() - len, sy), pos2(left.right(), sy)], Stroke::new(1.0, t.ruler_tick));
        if let Some(text) = label {
            // Vertical labels read top-to-bottom, one digit per line like Illustrator.
            let top = sy + 2.0;
            if top < filled_to {
                return;
            }
            filled_to = top + vertical_label_height(text.chars().count());
            for (k, ch) in text.chars().enumerate() {
                clip_left.text(
                    pos2(left.left() + 4.0, top + k as f32 * RULER_CHAR),
                    egui::Align2::LEFT_TOP,
                    ch.to_string(),
                    font.clone(),
                    t.ruler_tick,
                );
            }
        }
    });
    if let Some(h) = hover {
        let s = xf.to_screen(h);
        clip_top.line_segment([pos2(s.x, top.top()), pos2(s.x, top.bottom())], Stroke::new(1.0, t.text));
        clip_left.line_segment([pos2(left.left(), s.y), pos2(left.right(), s.y)], Stroke::new(1.0, t.text));
    }
}

fn to_screen_path(bp: &BezPath, xf: &Xf) -> Vec<Vec<Pos2>> {
    // Flatten for drawing: a polyline per subpath (tolerance ~0.25 screen px).
    let mut out: Vec<Vec<Pos2>> = vec![];
    let a = xf.affine();
    let mut t = bp.clone();
    t.apply_affine(a);
    let mut cur: Vec<Pos2> = vec![];
    kurbo_flatten(&t, 0.25, &mut |el| match el {
        PathEl::MoveTo(p) => {
            if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            cur.push(pos2(p.x as f32, p.y as f32));
        }
        PathEl::LineTo(p) => cur.push(pos2(p.x as f32, p.y as f32)),
        PathEl::ClosePath => {
            if let Some(f) = cur.first().copied() {
                cur.push(f);
            }
        }
        _ => {}
    });
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

fn stroke_path(p: &egui::Painter, bp: &BezPath, xf: &Xf, s: Stroke) {
    for line in to_screen_path(bp, xf) {
        p.add(Shape::line(line, s));
    }
}

fn c32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Outline of a node for highlighting (paths, compound children, area type frames, other
/// text/image bounds, symbol instances' bounds measured by their art from `symbols`; a clip
/// group's clipping path only, as [`walk_drawn`] visits it).
fn node_outline(n: &Node, symbols: &SymbolBoxes) -> BezPath {
    let mut bp = BezPath::new();
    walk_drawn(n, &mut |c| match &c.kind {
        NodeKind::Path { path, .. } => bp.extend(path.to_bezpath()),
        // Area type shows its frame: the type area Direct Selection reshapes; type on a path its
        // path, which the selection tools' brackets sit on; point type shows its baseline(s).
        NodeKind::Text(t) => {
            if c.extras().perspective.is_none() {
                if let Some(path) = t.area_frame().or_else(|| t.type_path()) {
                    bp.extend(path.to_bezpath());
                } else if matches!(t.kind, vectorcraft_doc::TextKind::Point) {
                    for (a, b) in t.baselines() {
                        bp.move_to(t.xf * a);
                        bp.line_to(t.xf * b);
                    }
                }
            } else if let Some(b) = c.geometric_bounds_with(symbols) {
                bp.extend(vectorcraft_geom::shapes::rectangle(b).to_bezpath());
            }
        }
        NodeKind::Image(_) | NodeKind::SymbolInstance { .. } => {
            if let Some(b) = c.geometric_bounds_with(symbols) {
                bp.extend(vectorcraft_geom::shapes::rectangle(b).to_bezpath());
            }
        }
        // A placed document shows its box, turned with it.
        NodeKind::PlacedDocument(p) => {
            let [a, b, c, d] = p.corners();
            bp.move_to(a);
            [b, c, d].into_iter().for_each(|q| bp.line_to(q));
            bp.close_path();
        }
        // A gradient mesh shows its mesh lines (#1111).
        NodeKind::Mesh(m) => bp.extend(m.lines().to_bezpath()),
        // An envelope shows its mesh (or top object), not its content.
        NodeKind::Envelope { .. } => {
            if let Some((lines, _)) = vectorcraft_doc::live::envelope_overlay(c) {
                bp.extend(lines.to_bezpath());
            }
        }
        _ => {}
    });
    bp
}

/// The width of the key object's outline (Align to Key Object), thicker than the selection's.
const KEY_OUTLINE: f32 = 2.5;

/// The most points ([`overlay_points`]) the selection's outlines and anchors, and the hover
/// highlight, are drawn for: an Image Trace of a photo selects hundreds of thousands of paths at
/// once, and selecting all of an opened PDF can select as many type objects, images and gradient
/// mesh pieces. Their outlines would make a mesh larger than the GPU takes in one buffer (#525,
/// #2460).
const OVERLAY_MAX_ANCHORS: usize = 100_000;

/// Whether `nodes` have more than [`OVERLAY_MAX_ANCHORS`] points to outline ([`overlay_points`];
/// counting stops there).
fn too_many_anchors<'a>(nodes: impl IntoIterator<Item = &'a Node>) -> bool {
    fn over(n: &Node, left: &mut usize) -> bool {
        match left.checked_sub(overlay_points(n)) {
            Some(l) => *left = l,
            None => return true,
        }
        // A clip group counts only its clipping path, the one child `walk_drawn` visits.
        if n.clips() {
            return n.children().and_then(|children| children.first()).is_some_and(|clip| over(clip, left));
        }
        !matches!(n.kind, NodeKind::Envelope { .. }) && n.editing_children().iter().any(|c| over(c, left))
    }
    let mut left = OVERLAY_MAX_ANCHORS;
    nodes.into_iter().any(|n| over(n, &mut left))
}

/// The points counted for `n` itself, without its children, toward [`OVERLAY_MAX_ANCHORS`]: a
/// path's anchors; the anchors of area type's frame or of type on a path's path; the four corners
/// of the box that area type or type on a path in perspective, images, symbol instances and
/// placed documents show; six for point type, its box's corners and the ends of the baseline
/// marker a selected one shows; and the points and the ends of the mesh lines of a gradient mesh
/// or an envelope.
fn overlay_points(n: &Node) -> usize {
    match &n.kind {
        NodeKind::Path { path, .. } => path.anchor_count(),
        NodeKind::Text(t) => match &t.kind {
            vectorcraft_doc::TextKind::Point => 6,
            _ if n.extras().perspective.is_some() => 4,
            vectorcraft_doc::TextKind::Area { frame } => frame.anchor_count(),
            vectorcraft_doc::TextKind::OnPath { path, .. } => path.anchor_count(),
        },
        NodeKind::Image(_) | NodeKind::SymbolInstance { .. } | NodeKind::PlacedDocument(_) => 4,
        NodeKind::Mesh(m) => m.lines().anchor_count() + m.points.len(),
        NodeKind::Envelope { .. } => {
            vectorcraft_doc::live::envelope_overlay(n).map_or(0, |(lines, grid)| lines.anchor_count() + grid.map_or(0, |g| g.points.len()))
        }
        _ => 0,
    }
}

/// [`Node::walk`] over what a selection highlight shows: an envelope's content is left out (the
/// envelope shows its mesh instead), and a clip group or clipping layer shows only its clipping
/// path, its first child, not the art the path clips (#2422).
fn walk_drawn<'a>(n: &'a Node, f: &mut impl FnMut(&'a Node)) {
    f(n);
    if matches!(n.kind, NodeKind::Envelope { .. }) {
        return;
    }
    if n.clips() {
        if let Some(clip) = n.children().and_then(|children| children.first()) {
            walk_drawn(clip, f);
        }
        return;
    }
    for c in n.editing_children().iter() {
        walk_drawn(c, f);
    }
}

/// The topmost editable object under document point `p` at `zoom`, as what is drawn there
/// ([`hit_with`]): a swatch or a symbol dropped on art.
fn hit_at(app: &VectorcraftApp, p: Point, zoom: f64) -> Option<vectorcraft_doc::hit::Hit> {
    hit_with(app, p, zoom, vectorcraft_doc::hit::hit_test)
}

/// The topmost editable object under document point `p` at `zoom`, as the selection tools pick
/// it: [`hit_at`], and a clipping path on its outline before the art it clips
/// ([`vectorcraft_doc::hit::HitOptions::clip_outlines`]). Object Highlighting and a right-click's
/// selection show and take what a click of those tools would.
fn selection_hit_at(app: &VectorcraftApp, p: Point, zoom: f64) -> Option<vectorcraft_doc::hit::Hit> {
    hit_with(app, p, zoom, |doc, p, opt| vectorcraft_doc::hit::hit_test(doc, p, vectorcraft_doc::hit::HitOptions { clip_outlines: true, ..opt }))
}

/// `test` on the active document at document point `p`, with the options the selection tools hit
/// objects with at `zoom` (Selection & Anchor Display › Tolerance and Object Selection by Path Only,
/// Type › Type Object Selection by Path Only, isolation mode).
fn hit_with<T>(
    app: &VectorcraftApp,
    p: Point,
    zoom: f64,
    test: impl FnOnce(&vectorcraft_doc::Document, Point, vectorcraft_doc::hit::HitOptions) -> Option<T>,
) -> Option<T> {
    let prefs = &app.session.prefs;
    let opt = vectorcraft_doc::hit::HitOptions {
        tol: prefs.selection_tolerance / zoom,
        outline: app.ui.view.outline,
        path_only: prefs.object_selection_by_path_only,
        type_path_only: prefs.type_selection_by_path_only,
        scope: app.session.active().and_then(|st| st.isolation),
        skip_guides: !app.ui.view.guides,
        ..Default::default()
    };
    test(&app.session.active()?.doc, p, opt)
}

/// Right-click: the object under the pointer is selected first unless it already is, then the
/// context menu lists what applies to the selection ([`crate::menus::context_items`]). The locked
/// objects under the pointer ([`vectorcraft_doc::hit::locked_objects_at`]) are kept in
/// `ui.context_locked` while the menu is open, for its Unlock submenu.
fn context_menu(app: &mut VectorcraftApp, resp: &egui::Response, xf: &Xf) {
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let p = xf.to_doc(p);
        let locked = hit_with(app, p, xf.zoom, |doc, p, opt| Some(vectorcraft_doc::hit::locked_objects_at(doc, p, opt)));
        app.ui.context_locked = app.session.active().map(|st| st.uid).zip(locked);
        if let Some(st) = app.session.active()
            && let Some(top) = selection_hit_at(app, p, xf.zoom).map(|h| h.top_object(st.isolation))
            && !st.selection.contains(top)
        {
            // A locked or hidden object can't be selected; the menu is then for the selection as is.
            let _ = app.run("select.set", json!({ "ids": [top.0] }));
        }
    }
    let mut clicked = None;
    if resp.context_menu(|ui| crate::menus::context_menu_body(app, ui, &mut clicked)).is_none() {
        app.ui.context_locked = None;
    }
    if let Some((id, p)) = clicked {
        crate::menus::invoke(app, &id, p);
    }
}

/// A panel drag ([`widgets::PanelDrag`]) dropped on art acts on the object under the pointer,
/// selected or not: a paint (swatches, a Fill/Stroke proxy, the Gradient panel's thumbnail) goes
/// to its active proxy (`paint.setFill`/`paint.setStroke` with its `ids`; a gradient fits it), the
/// Appearance panel's thumbnail gives the object (the topmost one hit) the appearance it carries
/// (`appearance.copyFrom`), a graphic style is applied to it (`graphicStyle.apply`; with Alt, on
/// top of its appearance). A chip follows the pointer meanwhile.
fn panel_drop(app: &mut VectorcraftApp, ui: &Ui, resp: &egui::Response, xf: &Xf) {
    crate::panels::swatches::drag_preview(app, ui.ctx());
    let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else { return };
    // Not through a floating panel over the canvas.
    if ui.ctx().layer_id_at(pos).is_some_and(|l| l != resp.layer_id) {
        return;
    }
    let Some(d) = resp.dnd_release_payload::<widgets::PanelDrag>() else { return };
    // A Libraries panel graphic: a copy centred where it is dropped.
    if let widgets::PanelDrag::LibraryGraphic { library, item } = &*d {
        let at = xf.to_doc(pos);
        if let Err(e) = app.run("library.use", json!({"library": library, "kind": "graphic", "item": item, "center": [at.x, at.y]})) {
            app.status(e);
        }
        return;
    }
    // A symbol from the Symbols panel: an instance centred where it is dropped, on art or not.
    if let widgets::PanelDrag::Symbol(name) = &*d {
        let at = xf.to_doc(pos);
        if let Err(e) = app.run("symbol.place", json!({"name": name, "x": at.x, "y": at.y})) {
            app.status(e);
        }
        return;
    }
    // A symbol from a symbol library panel: added to the document's Symbols and placed there.
    if let widgets::PanelDrag::LibrarySymbol { library, name } = &*d {
        let at = xf.to_doc(pos);
        if let Err(e) = app.run("symbol.addFromLibrary", json!({"library": library, "name": name, "place": true, "x": at.x, "y": at.y})) {
            app.status(e);
        }
        return;
    }
    let Some(hit) = hit_at(app, xf.to_doc(pos), xf.zoom) else { return };
    let Some(st) = app.session.active() else { return };
    let (cmd, params) = match &*d {
        widgets::PanelDrag::Paint { params, .. } => {
            // Colour groups paint nothing.
            if params.is_null() {
                return;
            }
            let mut params = params.clone();
            params["ids"] = json!([vectorcraft_tools::xform::paint_owner(&st.doc, hit.leaf).0]);
            params["focus"] = json!(false);
            (crate::panels::proxy_cmd(app, false), params)
        }
        widgets::PanelDrag::Appearance(source) => {
            let target = hit.top_object(st.isolation);
            if target == *source {
                return;
            }
            ("appearance.copyFrom", json!({"source": source.0, "ids": [target.0]}))
        }
        widgets::PanelDrag::GraphicStyle(name) => {
            let add = ui.input(|i| i.modifiers.alt);
            ("graphicStyle.apply", json!({"name": name, "ids": [hit.top_object(st.isolation).0], "add": add}))
        }
        // A brush from the Brushes panel: the path it lands on takes it (a compound path as a whole).
        widgets::PanelDrag::Brush { name, .. } => {
            ("brush.apply", json!({"name": name, "ids": [vectorcraft_tools::xform::paint_owner(&st.doc, hit.leaf).0]}))
        }
        // A brush from a brush library panel: added to the document's Brushes and applied to the
        // path it lands on, in one step.
        widgets::PanelDrag::LibraryBrush { library, name } => (
            "brush.addFromLibrary",
            json!({"library": library, "name": name, "apply": true, "ids": [vectorcraft_tools::xform::paint_owner(&st.doc, hit.leaf).0]}),
        ),
        // Art dragged back onto the canvas: its move was already dropped. (A symbol was placed
        // above.)
        widgets::PanelDrag::Art(_)
        | widgets::PanelDrag::Symbol(_)
        | widgets::PanelDrag::LibrarySymbol { .. }
        | widgets::PanelDrag::LibraryGraphic { .. } => return,
        // A Libraries panel style or group only moves within that panel.
        widgets::PanelDrag::LibraryItem { .. } | widgets::PanelDrag::LibraryGroup { .. } => return,
    };
    if let Err(e) = app.run(cmd, params) {
        app.status(e);
    }
}

fn hover_highlight(app: &VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let Some(h) = app.hover_doc else { return };
    // Smart Guides › Object Highlighting: a Smart Guides display option, so it needs them on and Snap to Grid and Pixel Preview off.
    if !app.ui.view.smart_guides || !app.session.prefs.object_highlighting || app.ui.view.snap_to_grid || app.ui.view.pixel_preview {
        return;
    }
    if app.session.tool_busy() || !matches!(app.session.tool_id(), "selection" | "directSelection" | "groupSelection") {
        return;
    }
    let Some(st) = app.session.active() else { return };
    let Some(hit) = selection_hit_at(app, h, xf.zoom) else { return };
    let id = if app.session.tool_id() == "selection" { hit.top_object(st.isolation) } else { hit.leaf };
    if st.selection.contains(id) {
        return;
    }
    if let Some(n) = st.doc.node(id).filter(|n| !too_many_anchors([*n])) {
        let color = c32(st.doc.layer_color(id));
        stroke_path(p, &node_outline(n, &st.doc.symbol_boxes()), xf, Stroke::new(1.5, color));
    }
}

/// Selected anchors are drawn slightly deeper than the layer colour (#4f80ff → #3d82ff for Layer 1).
fn selected_anchor(c: Color32) -> Color32 {
    if c == Color32::from_rgb(0x4f, 0x80, 0xff) { Color32::from_rgb(0x3d, 0x82, 0xff) } else { c }
}

/// How anchors and handles look: Selection & Anchor Display › Size and Handles.
#[derive(Clone, Copy)]
struct HandleLook<'a> {
    /// Points bigger (or smaller) than the default size 3, per step of Size (1–7).
    grow: f32,
    /// `solid` (the default), `hollow` or `large`.
    style: &'a str,
}

impl<'a> HandleLook<'a> {
    fn of(prefs: &'a vectorcraft_engine::Prefs) -> Self {
        Self { grow: prefs.anchor_size.clamp(1, 7) as f32 - 3.0, style: &prefs.handle_style }
    }

    /// A direction handle's end at `c`: a solid dot, a hollow one or a larger solid one.
    fn draw(self, p: &egui::Painter, c: Pos2, color: Color32) {
        let r = 2.75 + self.grow / 2.0;
        let hollow = self.style == "hollow";
        p.circle_filled(c, if self.style == "large" { r + 1.5 } else { r }, if hollow { Color32::WHITE } else { color });
        if hollow {
            p.circle_stroke(c, r, Stroke::new(1.0, color));
        }
    }
}

fn anchor_square(p: &egui::Painter, c: Pos2, color: Color32, filled: bool, size: f32) {
    let r = egui::Rect::from_center_size(c, vec2(size, size));
    if filled {
        p.rect_filled(r, 0.0, color);
    } else {
        p.rect_filled(r, 0.0, Color32::WHITE);
        p.rect_stroke(r, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
    }
}

fn curve_point(p: &egui::Painter, c: Pos2, color: Color32, filled: bool, size: f32) {
    p.circle(c, size / 2.0, if filled { color } else { Color32::WHITE }, Stroke::new(1.0, color));
}

/// Type → Show Hidden Characters: spaces as dots, ¶ at paragraph ends, # at the end of a story.
/// The marks of each text object are kept from one frame to the next ([`HiddenChars`]).
fn hidden_chars_overlay(app: &mut VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let Some(st) = app.session.active() else { return };
    let clip = p.clip_rect();
    let color = Color32::from_rgb(0x4f, 0x9d, 0xff);
    let font = egui::FontId::proportional(((10.0 * xf.zoom) as f32).clamp(7.0, 18.0));
    let kept = &mut app.canvas.hidden_chars;
    // `last` holds the marks of the last frame. The marks of the objects this frame doesn't draw
    // are dropped at the end.
    let mut last = std::mem::take(&mut kept.marks);
    // The inputs are read before any layout: when fonts load during this frame, the next frame
    // lays all type out again.
    let inputs = (vectorcraft_text::FontDb::global().generation(), vectorcraft_text::hyphenation_exceptions());
    if kept.inputs != inputs {
        last.clear();
        kept.inputs = inputs;
    }
    // `walk` visits each node's `Arc` in the order `Document::walk` visits the nodes (depth first,
    // in paint order).
    fn walk(nodes: &[std::sync::Arc<Node>], f: &mut impl FnMut(&std::sync::Arc<Node>)) {
        for n in nodes {
            f(n);
            if let Some(children) = n.children() {
                walk(children, f);
            }
        }
    }
    walk(&st.doc.layers, &mut |n| {
        let NodeKind::Text(tx) = &n.kind else { return };
        // The box round the object's corners on screen, turned with the view.
        if !n.visible || n.geometric_bounds().is_none_or(|b| !egui::Rect::from_points(&xf.quad(b)).intersects(clip)) {
            return;
        }
        let marks = match last.remove(&n.id) {
            Some((was, marks)) if std::sync::Arc::ptr_eq(&was, n) => marks,
            _ => std::sync::Arc::new(HiddenMarks::of(tx)),
        };
        for &at in &marks.spaces {
            p.circle_filled(xf.to_screen(at), 1.2, color);
        }
        for &(at, glyph) in &marks.ends {
            p.text(xf.to_screen(at), egui::Align2::LEFT_BOTTOM, glyph, font.clone(), color);
        }
        kept.marks.insert(n.id, (n.clone(), marks));
    });
}

/// The Show Hidden Characters marks of the text objects drawn in the last frame
/// ([`crate::CanvasCache::hidden_chars`]). Each is kept with the object it was laid out from: an
/// edited object is a new `Arc` and is laid out again, and all type is laid out again when the
/// fonts or the hyphenation exceptions change.
#[derive(Default)]
pub struct HiddenChars {
    /// The fonts' generation ([`vectorcraft_text::FontDb::generation`]) and the hyphenation
    /// exceptions the marks were laid out with.
    inputs: (u64, String),
    /// Each text object drawn, by id: the node it was laid out from and its marks.
    marks: std::collections::HashMap<vectorcraft_doc::NodeId, (std::sync::Arc<Node>, std::sync::Arc<HiddenMarks>)>,
}

/// One text object's marks in document space: a dot over each space, and ¶ or # after the lines
/// that end a paragraph or the story. Type on a path has none.
#[derive(Default)]
struct HiddenMarks {
    spaces: Vec<Point>,
    ends: Vec<(Point, &'static str)>,
}

impl HiddenMarks {
    /// Lay out `tx` and place its marks.
    fn of(tx: &vectorcraft_doc::TextObject) -> Self {
        let mut marks = Self::default();
        let lay = vectorcraft_text::layout(vectorcraft_text::FontDb::global(), tx);
        if lay.on_path {
            return marks;
        }
        let text = tx.plain_text();
        let to = |q: Point| tx.xf * q;
        // A list marker's glyphs (length 0) stand for no character.
        for g in lay.glyphs.iter().filter(|g| g.len > 0) {
            if text.get(g.byte..).is_some_and(|s| s.starts_with(' ')) {
                marks.spaces.push(to(Point::new(g.origin.x + g.advance / 2.0, g.origin.y - 3.0)));
            }
        }
        for (i, line) in lay.lines.iter().enumerate() {
            let last = i + 1 == lay.lines.len();
            let breaks = text.get(line.end..).is_some_and(|s| s.starts_with('\n'));
            let (glyph, at) = if last && line.end >= text.len() {
                ("#", true)
            } else if breaks {
                ("¶", true)
            } else {
                ("", false)
            };
            if at {
                marks.ends.push((to(Point::new(line.x1 + 1.0, line.baseline)), glyph));
            }
        }
        marks
    }
}

/// View › Show Text Threads: for each thread with a selected frame, a line in its first frame's
/// layer colour from each frame's out port to the next frame's in port, both showing the arrow of a
/// threaded port, where the tools draw the ports of selected type ([`vectorcraft_tools::text`]).
/// Only frames with ports ([`vectorcraft_tools::text::ported`]) are joined: a link to or from a
/// frame in perspective, which has none, gets no line. With more than
/// [`vectorcraft_tools::text::PORTS_MAX`] such frames selected the tools draw no ports, and only
/// the lines are drawn here. `zoom`: screen pixels to the document unit.
fn thread_overlays(app: &VectorcraftApp, zoom: f64) -> Vec<Overlay> {
    use vectorcraft_tools::text::{Port, PortState, port_centre, ported, ports_shown, push_port};
    let Some(st) = app.session.active() else { return vec![] };
    let px = 1.0 / zoom.max(1e-9);
    let boxes = ports_shown(&st.doc, &st.selection.objects);
    let mut o = vec![];
    for thread in &st.doc.text_threads {
        if !thread.iter().any(|id| st.selection.objects.contains(id)) {
            continue;
        }
        let color = st.doc.layer_color(thread[0]);
        for w in thread.windows(2) {
            let [a, b] = [w[0], w[1]];
            let (Some(from), Some(to)) = (ported(&st.doc, a), ported(&st.doc, b)) else { continue };
            let (Some(out), Some(input)) = (port_centre(from, Port::Out, px), port_centre(to, Port::In, px)) else { continue };
            o.push(Overlay::Line { a: out, b: input, color, dashed: false });
            if boxes {
                push_port(&mut o, out, PortState::Threaded, st.doc.layer_color(a), px);
                push_port(&mut o, input, PortState::Threaded, st.doc.layer_color(b), px);
            }
        }
    }
    o
}

/// View → Show Live Paint Gaps: the gaps of the selected Live Paint groups in each group's Gap
/// Preview Color ([`crate::dialogs::live_paint_gaps::selected_gaps`]). Cached per revision.
fn live_paint_gap_overlays(app: &mut VectorcraftApp) -> Vec<Overlay> {
    let Some(st) = app.session.active() else { return vec![] };
    let key = (st.uid, st.revision);
    let gaps = match &app.canvas.live_paint_gaps {
        Some((k, gaps)) if *k == key => gaps.clone(),
        _ => {
            let gaps = std::sync::Arc::new(crate::dialogs::live_paint_gaps::selected_gaps(app));
            app.canvas.live_paint_gaps = Some((key, gaps.clone()));
            gaps
        }
    };
    gaps.to_vec()
}

/// Simplify's Show Original Path: the paths its preview ran on, as they were before it, in a band
/// 3 screen points wide of their layer's color at 40%, under the selection's outlines. Nothing is drawn
/// when they have more than [`OVERLAY_MAX_ANCHORS`] anchor points.
fn original_path_overlay(app: &VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let paths = crate::dialogs::path_ops::original_paths(app);
    let Some(st) = app.session.active().filter(|_| paths.iter().map(|(_, path)| path.anchor_count()).sum::<usize>() <= OVERLAY_MAX_ANCHORS) else {
        return;
    };
    for (id, path) in paths {
        stroke_path(p, &path.to_bezpath(), xf, Stroke::new(3.0, c32(st.doc.layer_color(id)).gamma_multiply(0.4)));
    }
}

/// The slices (unless View → Hide Slices): user and object slices outlined in the Slices
/// preferences' line colour, auto slices dashed and dimmer, each numbered at its top left (Show
/// Slice Numbers); selected slices have a bolder outline. The layout is cached per revision.
fn slice_overlay(app: &mut VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let Some(st) = app.session.active() else { return };
    let key = (st.uid, st.revision);
    let slices = match &app.canvas.slices {
        Some((k, s)) if *k == key => s.clone(),
        _ => {
            let s = std::sync::Arc::new(st.doc.slice_layout());
            app.canvas.slices = Some((key, s.clone()));
            s
        }
    };
    if slices.is_empty() {
        return;
    }
    let prefs = &app.session.prefs;
    let line = pref_color(&prefs.slice_line_color, Color32::from_rgb(0xff, 0x3f, 0x3f));
    let dim = line.gamma_multiply(0.55);
    let selected = &st.selection.slices;
    let objects = &st.selection.objects;
    for a in slices.iter() {
        let auto = a.source == vectorcraft_doc::SliceSource::Auto;
        let on = a.id.is_some_and(|id| selected.contains(&id) || objects.contains(&id));
        let q = xf.quad(a.rect);
        let ring: Vec<Pos2> = q.iter().chain(q.first()).copied().collect();
        if auto {
            p.extend(Shape::dashed_line(&ring, Stroke::new(1.0, dim), 3.0, 3.0));
        } else {
            p.add(Shape::line(ring, Stroke::new(if on { 2.0 } else { 1.0 }, line)));
        }
        if prefs.show_slice_numbers
            && let Some(tl) = q.first()
        {
            let font = egui::FontId::proportional(9.0);
            let galley = p.layout_no_wrap(format!("{:02}", a.number), font, Color32::WHITE);
            let badge = egui::Rect::from_min_size(*tl + vec2(1.0, 1.0), galley.size() + vec2(6.0, 2.0));
            p.rect_filled(badge, CornerRadius::ZERO, if auto { dim } else { line });
            p.galley(badge.min + vec2(3.0, 1.0), galley, Color32::WHITE);
        }
    }
}

/// The print tiling (View → Show Print Tiling, or the Print Tiling tool): each page of the
/// document's print settings as `print.preview` lays it out, its paper edge and its imageable
/// area dashed, numbered at its top left; tiles outside the tile range dimmer. Cached per revision.
fn print_tiling_overlay(app: &mut VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let Some(st) = app.session.active() else { return };
    let key = (st.uid, st.revision);
    let pages = match &app.canvas.print_tiling {
        Some((k, pages)) if *k == key => pages.clone(),
        _ => {
            // Settings that can't print have no pages to show.
            let pages = std::sync::Arc::new(vectorcraft_engine::cmd::printtiling::pages(&st.doc).unwrap_or_default());
            app.canvas.print_tiling = Some((key, pages.clone()));
            pages
        }
    };
    let font = egui::FontId::proportional(10.0);
    let ring = |r: [f64; 4]| {
        let q = xf.quad(Rect::new(r[0], r[1], r[2], r[3]));
        q.iter().chain(q.first()).copied().collect::<Vec<Pos2>>()
    };
    for page in pages.iter() {
        let color = if page.printed { PRINT_TILING } else { PRINT_TILING.gamma_multiply(0.45) };
        p.add(Shape::line(ring(page.page), Stroke::new(0.75, color)));
        let inner = ring(page.imageable);
        p.extend(Shape::dashed_line(&inner, Stroke::new(1.0, color), 4.0, 3.0));
        if let Some(corner) = inner.iter().copied().reduce(|a, b| pos2(a.x.min(b.x), a.y.min(b.y))) {
            p.text(corner + vec2(3.0, 2.0), egui::Align2::LEFT_TOP, page.number.to_string(), font.clone(), color);
        }
    }
}

/// The print tiling's lines: a neutral grey that reads on the paper and on the pasteboard.
const PRINT_TILING: Color32 = Color32::from_gray(96);

/// A Shaper Group's mode widget (Shaper and Selection tools), drawn with selection edges shown or
/// hidden: an arrow pointing right in Face Selection mode and left in Construction mode.
fn shaper_widget_overlay(app: &VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    let Some(st) = app.session.active() else { return };
    if matches!(app.session.tool_id(), "shaper" | "selection")
        && let Some(w) = vectorcraft_tools::shaperwidget::ShaperWidget::of(&st.doc, &st.selection, st.isolation, xf.zoom)
    {
        let (c, r) = (xf.to_screen(w.at), vectorcraft_tools::shaperwidget::RADIUS_PX);
        let color = c32(st.doc.layer_color(w.group));
        p.circle_filled(c, r, Color32::WHITE);
        p.circle_stroke(c, r, Stroke::new(1.0, color));
        let d = if w.construction { -1.0 } else { 1.0 };
        let arrow = vec![c + vec2(3.0 * d, 0.0), c + vec2(-2.0 * d, -3.0), c + vec2(-2.0 * d, 3.0)];
        p.add(Shape::convex_polygon(arrow, color, Stroke::NONE));
    }
}

/// High-contrast live controls, using the same geometry as the Selection tool's hit testing.
fn mirror_axis_overlay(app: &VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    if app.session.tool_id() != "selection" || !app.ui.view.bounding_box {
        return;
    }
    let Some(st) = app.session.active() else { return };
    let Some(axis) = vectorcraft_tools::mirror::MirrorAxis::of(&st.doc, &st.selection, xf.zoom) else { return };
    let t = Tokens::get(p.ctx());
    let ends = axis.ends.map(|q| xf.to_screen(q));
    let center = xf.to_screen(axis.center);
    if !center.is_finite() || !ends.iter().all(|p| p.is_finite()) {
        return;
    }
    if let Some((a, b)) = clip_segment(ends[0], ends[1], xf.rect) {
        p.line_segment([a, b], Stroke::new(5.0, t.measure_bg));
        p.line_segment([a, b], Stroke::new(2.0, t.selection));
    }
    for end in ends {
        p.circle_filled(end, vectorcraft_tools::mirror::HANDLE_RADIUS, t.measure_bg);
        p.circle_stroke(end, vectorcraft_tools::mirror::HANDLE_RADIUS, Stroke::new(2.0, t.selection));
    }
    let handle = egui::Rect::from_center_size(center, vec2(12.0, 12.0));
    p.rect_filled(handle, 2.0, t.selection);
    p.rect_stroke(handle, 2.0, Stroke::new(2.0, t.measure_bg), StrokeKind::Outside);
}

/// A selected grid repeat's row, column and corner handles and its spacing sliders (#2691), at
/// the places the Selection tool's hit testing finds them.
fn grid_repeat_overlay(app: &mut VectorcraftApp, p: &egui::Painter, xf: &Xf) {
    use vectorcraft_tools::repeat_grid::{GridHandles, GridPart, HANDLE_RADIUS, SLIDER_HALF};
    if app.session.tool_id() != "selection" || !app.ui.view.bounding_box {
        return;
    }
    let frame = app.selection_box();
    let Some(st) = app.session.active() else { return };
    let Some(grid) = GridHandles::of(&st.doc, &st.selection, frame, xf.zoom) else { return };
    let t = Tokens::get(p.ctx());
    let half = f64::from(SLIDER_HALF) / xf.zoom;
    for (part, at) in grid.parts() {
        let c = xf.to_screen(at);
        if !c.is_finite() {
            continue;
        }
        if part.is_slider() {
            // A bar across the gap's middle, square to the spacing it sets.
            let along = if part == GridPart::HSpacing { vectorcraft_geom::Vec2::new(0.0, half) } else { vectorcraft_geom::Vec2::new(half, 0.0) };
            let ends = [xf.to_screen(at - along), xf.to_screen(at + along)];
            p.line_segment(ends, Stroke::new(6.0, t.measure_bg));
            p.line_segment(ends, Stroke::new(2.5, t.selection));
        } else {
            p.circle_filled(c, HANDLE_RADIUS, t.measure_bg);
            p.circle_stroke(c, HANDLE_RADIUS, Stroke::new(2.0, t.selection));
        }
    }
}

/// The radius of the Rotate View dial, in screen points.
const DIAL_RADIUS: f32 = 40.0;

/// The dial a Rotate View drag shows, centered on `c`: a tick every 15° (longer every 90°), a
/// needle pointing where the document's top points in a view turned by `rotation` degrees, and the
/// angle under the center, rounded to whole degrees as in the status bar.
fn rotate_view_dial(p: &egui::Painter, c: Pos2, rotation: f64, t: &Tokens) {
    // `dir(deg)` is the screen direction `deg` degrees clockwise from straight up.
    let dir = |deg: f32| vec2(deg.to_radians().sin(), -deg.to_radians().cos());
    p.circle(c, DIAL_RADIUS, t.panel.gamma_multiply(0.85), Stroke::new(1.0, t.border));
    for i in 0..24u8 {
        let len = if i % 6 == 0 { 8.0 } else { 4.0 };
        let d = dir(f32::from(i) * 15.0);
        p.line_segment([c + d * (DIAL_RADIUS - len), c + d * DIAL_RADIUS], Stroke::new(1.0, t.text_dim));
    }
    p.line_segment([c, c + dir(rotation as f32) * DIAL_RADIUS], Stroke::new(2.0, t.accent));
    p.circle_filled(c, 2.5, t.accent);
    p.text(c + vec2(0.0, 8.0), egui::Align2::CENTER_TOP, format!("{rotation:.0}°"), egui::FontId::proportional(11.0), t.text);
}

fn selection_overlay(app: &mut VectorcraftApp, p: &egui::Painter, xf: &Xf, curve_points_from_tool: bool) {
    // The Selection tool's bounding box (rotated with the objects after a rotation). It hides while
    // a drag moves or resizes the selection, so only the art is seen going with the pointer (#712).
    let show_box = app.session.tool_id() == "selection"
        && !app.session.tool_transforming()
        && app.ui.view.bounding_box
        && app.session.active().is_some_and(|st| !st.selection.is_empty() && st.selection.anchors.is_empty());
    let bbox = if show_box { app.selection_box() } else { None };
    let st = app.session.active();
    let big = st.is_some_and(|st| too_many_anchors(st.selection.objects.iter().filter_map(|id| st.doc.node(*id))));
    let big_bounds = if big { app.selection_bounds() } else { None };
    let app = &*app;
    let Some(st) = app.session.active() else { return };
    let symbols = st.doc.symbol_boxes();
    let tool = app.session.tool_id();
    let direct = vectorcraft_tools::catalog::edits_anchors(tool);
    let curvature = tool == "curvature";
    // Selection & Anchor Display › Size (1–7, 3 the default): anchors, handles and the bounding
    // box's handles a point bigger or smaller per step.
    let look = HandleLook::of(&app.session.prefs);
    let grow = look.grow;
    let anchor = |direct: bool| grow + if direct { 5.0 } else { 4.0 };
    // The selected anchors' handles (drawn once they are counted: Show handles when multiple
    // anchors are selected off shows them for a single one only).
    let (mut handles, mut with_handles) = (vec![], 0);
    // Too many paths to outline: the selection's bounds stand for them.
    if let Some((b, id)) = big_bounds.zip(st.selection.objects.first()) {
        stroke_path(p, &vectorcraft_geom::shapes::rectangle(b).to_bezpath(), xf, Stroke::new(1.0, c32(st.doc.layer_color(*id))));
    }
    // The key object (Align to Key Object): its outline drawn thicker, or its bounds when it has
    // too many paths to outline.
    if let Some((k, n)) = st.selection.key.and_then(|k| Some((k, st.doc.node(k)?))) {
        let outline = if too_many_anchors([n]) {
            n.geometric_bounds_with(&symbols).map(|b| vectorcraft_geom::shapes::rectangle(b).to_bezpath())
        } else {
            Some(node_outline(n, &symbols))
        };
        if let Some(bp) = outline {
            stroke_path(p, &bp, xf, Stroke::new(KEY_OUTLINE, c32(st.doc.layer_color(k))));
        }
    }
    for id in st.selection.objects.iter().filter(|_| !big) {
        let Some(n) = st.doc.node(*id) else { continue };
        let color = c32(st.doc.layer_color(*id));
        let partial = st.selection.partial(*id);
        // Path outlines (and area type / placed document / image / mesh / symbol outlines;
        // point type baselines and origin are drawn below).
        if !matches!(n.kind, NodeKind::Text(ref tx) if matches!(tx.kind, vectorcraft_doc::TextKind::Point)) {
            stroke_path(p, &node_outline(n, &symbols), xf, Stroke::new(1.0, color));
        }
        // Anchors (and handles for selected anchors in direct mode); a gradient mesh's or a mesh
        // envelope's points.
        walk_drawn(n, &mut |c| {
            let envelope = vectorcraft_doc::live::envelope_overlay(c).and_then(|(_, grid)| grid);
            let grid = match &c.kind {
                NodeKind::Mesh(m) => Some(m),
                _ => envelope.as_ref(),
            };
            if let Some(grid) = grid {
                for q in &grid.points {
                    anchor_square(p, xf.to_screen(q.p), color, false, anchor(direct));
                }
                return;
            }
            let NodeKind::Path { path, .. } = &c.kind else { return };
            // The Curvature tool owns its points while drawing; existing paths use this overlay.
            if curvature && curve_points_from_tool {
                return;
            }
            let selected_segments = st.selection.segments.get(&c.id);
            if let Some(segments) = selected_segments {
                for &(si, seg) in segments {
                    let Some(sp) = path.subpaths.get(si).filter(|sp| seg < sp.segment_count()) else { continue };
                    let curve = sp.segment(seg);
                    let mut outline = vectorcraft_geom::BezPath::new();
                    outline.move_to(curve.p0);
                    if sp.segment_is_line(seg) {
                        outline.line_to(curve.p3);
                    } else {
                        outline.curve_to(curve.p1, curve.p2, curve.p3);
                    }
                    stroke_path(p, &outline, xf, Stroke::new(2.0, color));
                }
            }
            for (si, ai, a) in path.anchors() {
                // An object selected whole has all its anchors selected, and so do the paths inside a
                // group, compound path or clip group selected whole. Direction handles show only on a
                // path that is selected itself: those are the handles that `handle_anchors` in
                // `vectorcraft-tools` lets the tools drag.
                let (sel, handles_shown) = match partial {
                    Some(set) => {
                        let sel = set.contains(&(si, ai));
                        (sel, sel)
                    }
                    None => (true, direct && c.id == *id),
                };
                let sp = xf.to_screen(a.p);
                if !curvature && handles_shown {
                    with_handles += 1;
                    let sides = path.subpaths.get(si).map(|subpath| st.selection.handle_sides(c.id, si, ai, subpath)).unwrap_or((true, true));
                    handles.extend(
                        [(a.h_in, sides.0), (a.h_out, sides.1)]
                            .into_iter()
                            .filter(|(h, show)| *show && h.distance(a.p) > 1e-6)
                            .map(|(h, _)| (sp, xf.to_screen(h), color)),
                    );
                }
                let anchor_selected = sel && selected_segments.is_none();
                let ink = if anchor_selected && partial.is_some() { selected_anchor(color) } else { color };
                if curvature {
                    curve_point(p, sp, ink, anchor_selected && partial.is_some(), grow + 6.0);
                } else {
                    anchor_square(p, sp, ink, anchor_selected, anchor(partial.is_some() || direct));
                }
            }
        });
        // Centre point (Attributes panel → Show Center).
        if n.shows_center()
            && let Some(b) = n.geometric_bounds_with(&symbols)
        {
            anchor_square(p, xf.to_screen(b.center()), color, true, anchor(false));
        }
        // Point type: each line's baseline, where a click selects it (`hit`), whatever its
        // alignment and rotation, and the anchor (area type shows its frame instead, which may
        // not be a rectangle, and type on a path its path).
        if let NodeKind::Text(tx) = &n.kind
            && matches!(tx.kind, vectorcraft_doc::TextKind::Point)
        {
            for (a, b) in tx.baselines() {
                p.line_segment([xf.to_screen(tx.xf * a), xf.to_screen(tx.xf * b)], Stroke::new(1.0, color));
            }
            p.circle_filled(xf.to_screen(tx.xf * Point::ZERO), 2.5, color);
        }
    }
    if app.session.prefs.show_handles_multiple_anchors || with_handles <= 1 {
        for (sp, hp, color) in handles {
            p.line_segment([sp, hp], Stroke::new(1.0, color));
            look.draw(p, hp, color);
        }
    }
    // The spine of each selected blend (or of the blend a selected key object belongs to).
    let mut spines = vec![];
    for id in &st.selection.objects {
        let is_blend = |b: &vectorcraft_doc::NodeId| st.doc.node(*b).is_some_and(|n| matches!(n.kind, NodeKind::Blend { .. }));
        let Some(b) = [Some(*id), st.doc.parent_of(*id)].into_iter().flatten().find(is_blend).filter(|b| !spines.contains(b)) else { continue };
        spines.push(b);
        let Some(NodeKind::Blend { children, spec }) = st.doc.node(b).map(|n| &n.kind) else { continue };
        let Some((path, _)) = vectorcraft_doc::live::blend_spine(children, spec) else { continue };
        let color = c32(st.doc.layer_color(b));
        stroke_path(p, &path.to_bezpath(), xf, Stroke::new(1.0, color));
        for (_, _, a) in path.anchors() {
            anchor_square(p, xf.to_screen(a.p), color, false, anchor(direct));
        }
    }
    // Live Corners widgets (Selection on a live rectangle or polygon, Direct Selection on any path).
    if matches!(tool, "selection" | "directSelection")
        && let Some(w) = vectorcraft_tools::corners::CornerWidgets::showing(
            &st.doc,
            &st.selection,
            xf.zoom,
            tool == "directSelection",
            app.ui.view.corner_widgets,
            app.session.prefs.hide_corner_widget_above,
        )
    {
        for (id, q) in w.visible_on() {
            let (sp, color) = (xf.to_screen(q), c32(st.doc.layer_color(id)));
            p.circle_filled(sp, 3.0, Color32::WHITE);
            p.circle_stroke(sp, 3.0, Stroke::new(1.0, color));
            p.circle_filled(sp, 1.0, color);
        }
    }
    // Bounding box with handles (Selection tool).
    if let Some(b) = bbox {
        let color = c32(st.doc.layer_color(st.selection.objects[0]));
        p.add(Shape::closed_line(b.corners().iter().map(|q| xf.to_screen(*q)).collect(), Stroke::new(1.0, color)));
        for h in vectorcraft_tools::bbox::Handle::ALL {
            let c = xf.to_screen(b.to_doc() * h.pos(b.rect));
            let hr = egui::Rect::from_center_size(c, vec2(6.0 + grow, 6.0 + grow));
            p.rect_filled(hr, 0.0, Color32::WHITE);
            p.rect_stroke(hr, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
        }
        // The type widget beside it: hollow on point type, filled on area type.
        if let Some(w) = vectorcraft_tools::typewidget::TypeWidget::of(&st.doc, &st.selection, &b, xf.zoom) {
            let (c, r) = (xf.to_screen(w.at), vectorcraft_tools::typewidget::RADIUS_PX);
            p.circle_filled(c, r, if w.area { color } else { Color32::WHITE });
            p.circle_stroke(c, r, Stroke::new(1.0, color));
        }
    }
    // Pie widgets (Selection tool, with View › Show Bounding Box, on live ellipses): each joined
    // by a line to the outline's point at its angle. They stay while one is dragged.
    if tool == "selection" && app.ui.view.bounding_box {
        for w in vectorcraft_tools::pie::PieWidget::of(&st.doc, &st.selection, xf.zoom) {
            let (on, c, r, color) = (xf.to_screen(w.on), xf.to_screen(w.at), vectorcraft_tools::pie::RADIUS_PX, c32(st.doc.layer_color(w.id)));
            p.line_segment([on, c], Stroke::new(1.0, color));
            p.circle_filled(c, r, Color32::WHITE);
            p.circle_stroke(c, r, Stroke::new(1.0, color));
        }
    }
}

/// Record where the Type tool's IME candidate window goes this pass, if it has an editor.
///
/// The descriptor itself is observed once at the end of the pass (`crate::ime::end_of_pass`),
/// after dialogs, floating panels, progress windows and the palette draw: any of them can open
/// over the canvas, take the keyboard or run a command that changes the edited text, so a
/// descriptor written here would carry a stale owner, surrounding text or cause.
fn ime_geometry(app: &mut VectorcraftApp, ctx: &egui::Context, xf: &Xf) {
    app.ime_caret_rect = None;
    // A hidden viewport cannot compose; the end-of-pass observation clears the owner.
    if !app.session.tool_wants_text() || app.viewport_hidden(ctx) {
        return;
    }
    let Some((a, b)) = app.session.tool_ime_caret(app.view_info()) else { return };
    let r = egui::Rect::from_two_pos(xf.to_screen(a), xf.to_screen(b)).expand2(vec2(1.0, 0.0));
    app.ime_caret_rect = Some((r, r));
}

/// The width an artboard needs on screen for its name to show (px): narrower, the name is left
/// out rather than spilling over its neighbours.
const MIN_LABELLED_ARTBOARD: f32 = 24.0;

/// Temp-memory key recording an artboard-name press before its edit field opens.
fn name_press_id() -> egui::Id {
    egui::Id::unique("canvas-artboard-name-press")
}

/// Temp-memory key for the artboard name field currently being edited.
fn name_edit_id() -> egui::Id {
    egui::Id::unique("canvas-artboard-name-edit")
}

/// A name being typed above an artboard. `fresh`: this frame opened it, so a focus loss from the
/// press that started it is not the user leaving the field.
#[derive(Clone)]
struct ArtboardNameEdit {
    index: usize,
    text: String,
    fresh: bool,
}

/// The canvas label for `name`, laid out as [`artboard_label`] draws it.
fn canvas_name_galley(painter: &egui::Painter, corner: Pos2, width: f32, name: &str, color: Color32) -> Option<(std::sync::Arc<egui::Galley>, Pos2)> {
    if !width.is_finite() || width < MIN_LABELLED_ARTBOARD {
        return None;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_owned(), egui::FontId::proportional(11.0), color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = painter.layout_job(job);
    let at = corner - vec2(0.0, 4.0 + galley.size().y);
    Some((galley, at))
}

/// The length on screen, in points, of each arm of an artboard's center mark (a cross), at any zoom.
const CENTER_MARK_ARM: f32 = 6.0;

/// Artboard `ab`'s display marks in `color`, each line 1 point wide on screen at any zoom and
/// turned with the view: with Show Cross Hairs, a line from the middle of each side through the
/// center to the middle of the opposite side; with Show Center Mark, a cross at the center whose
/// arms run along the artboard's sides, [`CENTER_MARK_ARM`] long on screen
/// (https://helpx.adobe.com/illustrator/desktop/create-manage-artboards/organize-manage-artboards/modify-display-settings-of-artboards.html);
/// with Show Video Safe Areas, the outlines of its action-safe and title-safe areas
/// ([`vectorcraft_doc::Artboard::video_safe_areas`],
/// https://helpx.adobe.com/illustrator/desktop/create-manage-artboards/organize-manage-artboards/set-video-display-options-for-artboards.html).
fn artboard_marks(painter: &egui::Painter, xf: &Xf, ab: &vectorcraft_doc::Artboard, color: Color32) {
    let (r, stroke) = (ab.rect, Stroke::new(1.0, color));
    let c = r.center();
    let at = |x: f64, y: f64| xf.to_screen(Point::new(x, y));
    if ab.show_cross_hairs {
        painter.line_segment([at(r.x0, c.y), at(r.x1, c.y)], stroke);
        painter.line_segment([at(c.x, r.y0), at(c.x, r.y1)], stroke);
    }
    if ab.show_center_mark {
        let mid = at(c.x, c.y);
        for side in [at(c.x + 1.0, c.y), at(c.x, c.y + 1.0)] {
            let arm = (side - mid).normalized() * CENTER_MARK_ARM;
            painter.line_segment([mid - arm, mid + arm], stroke);
        }
    }
    if ab.show_video_safe_areas {
        for area in ab.video_safe_areas() {
            painter.add(Shape::closed_line(xf.quad(area), stroke));
        }
    }
}

/// Artboard `name` above its top-left `corner` on screen, no wider than the artboard (`width` on
/// screen): a longer name ends in "…", and an artboard too narrow shows none (#949).
fn artboard_label(painter: &egui::Painter, corner: Pos2, width: f32, name: &str, color: Color32) {
    if let Some((galley, at)) = canvas_name_galley(painter, corner, width, name, color) {
        painter.galley(at, galley, color);
    }
}

fn canvas_name_rect(painter: &egui::Painter, corner: Pos2, width: f32, name: &str) -> Option<egui::Rect> {
    canvas_name_galley(painter, corner, width, name, Color32::WHITE).map(|(galley, at)| egui::Rect::from_min_size(at, galley.size()).expand(2.0))
}

/// Where the Artboard tool draws "01 - name" for its active artboard (see its overlay label).
fn tool_name_rect(painter: &egui::Painter, xf: &Xf, rect: Rect, index: usize, name: &str) -> egui::Rect {
    let z = xf.zoom.max(1e-9);
    let anchor = Point::new(rect.x0 - 8.0 / z, rect.y0 - 4.0 / z);
    let sp = xf.to_screen(anchor) + vec2(8.0, -14.0);
    let galley = painter.layout(format!("{:02} - {name}", index + 1), egui::FontId::proportional(11.0), Color32::WHITE, f32::INFINITY);
    egui::Rect::from_min_size(sp, galley.size()).expand(2.0)
}

/// The artboard whose on-canvas name contains `screen`, if that name is being drawn.
fn artboard_name_at(app: &VectorcraftApp, ui: &Ui, xf: &Xf, screen: Pos2) -> Option<usize> {
    let st = app.session.active()?;
    let artboard_tool = app.session.tool_id() == "artboard";
    if !artboard_tool && st.doc.artboards.len() <= 1 {
        return None;
    }
    let tool_labelled = artboard_tool.then(|| app.session.tool_options()["active"].as_u64()).flatten();
    let painter = ui.painter();
    let mut hit = None;
    for (i, ab) in st.doc.artboards.iter().enumerate() {
        if tool_labelled == u64::try_from(i).ok() {
            continue;
        }
        let corner = xf.to_screen(Point::new(ab.rect.x0, ab.rect.y0));
        let width = (ab.rect.width() * xf.zoom) as f32;
        let text = format!("{:02} - {}", i + 1, ab.name);
        if canvas_name_rect(painter, corner, width, &text).is_some_and(|r| r.contains(screen)) {
            hit = Some(i);
        }
    }
    if let Some(i) = tool_labelled.and_then(|i| usize::try_from(i).ok())
        && let Some(ab) = st.doc.artboards.get(i)
        && tool_name_rect(painter, xf, ab.rect, i, &ab.name).contains(screen)
    {
        return Some(i);
    }
    hit
}

fn select_named_artboard(app: &mut VectorcraftApp, index: usize) {
    let n = app.session.active().map(|st| st.doc.artboards.len()).unwrap_or(0);
    if index >= n {
        return;
    }
    if let Some(v) = app.view_mut() {
        v.artboard = index;
    }
    if app.session.tool_id() == "artboard" {
        app.session.set_tool_option("active", &json!(index));
    }
}

/// Start editing artboard `index`'s name on the canvas, unless the artboard is locked (the
/// Artboards panel still renames it).
fn begin_artboard_name_edit(ui: &Ui, app: &VectorcraftApp, index: usize) {
    let Some(name) = app.session.active().and_then(|st| st.doc.artboards.get(index).filter(|a| !a.locked).map(|a| a.name.clone())) else { return };
    ui.data_mut(|d| d.insert_temp(name_edit_id(), ArtboardNameEdit { index, text: name, fresh: true }));
}

fn commit_artboard_name(app: &mut VectorcraftApp, index: usize, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let current = app.session.active().and_then(|st| st.doc.artboards.get(index).map(|a| a.name.clone()));
    if current.as_deref() == Some(trimmed) {
        return;
    }
    // A refusal is reported by the command; the label just closes.
    app.run("artboard.setProps", json!({ "index": index, "name": trimmed })).ok();
}

/// The text field over an artboard name a double-click opened.
fn artboard_name_editor(app: &mut VectorcraftApp, ui: &mut Ui, xf: &Xf) {
    let Some(mut edit) = ui.data(|d| d.get_temp::<ArtboardNameEdit>(name_edit_id())) else { return };
    let alive = app.session.active().is_some_and(|st| edit.index < st.doc.artboards.len());
    if !alive {
        ui.data_mut(|d| d.remove::<ArtboardNameEdit>(name_edit_id()));
        return;
    }
    let Some(rect) = artboard_name_field_rect(app, ui, xf, edit.index) else {
        ui.data_mut(|d| d.remove::<ArtboardNameEdit>(name_edit_id()));
        return;
    };
    let fresh = edit.fresh;
    edit.fresh = false;
    let te = ui.put(
        rect,
        egui::TextEdit::singleline(&mut edit.text)
            .id(name_edit_id().with("field"))
            .font(egui::FontId::proportional(11.0))
            // Keep the original inset so theme button padding cannot move the canvas label.
            .margin(egui::Margin::symmetric(4, 2)),
    );
    if fresh || (!te.has_focus() && !te.lost_focus()) {
        te.request_focus();
    }
    let done = !fresh && te.lost_focus();
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let (index, text) = (edit.index, edit.text.clone());
    if done {
        ui.data_mut(|d| d.remove::<ArtboardNameEdit>(name_edit_id()));
        if !escape {
            commit_artboard_name(app, index, &text);
        }
    } else {
        ui.data_mut(|d| d.insert_temp(name_edit_id(), edit));
    }
}

/// Where the rename field sits: the name label's rectangle, at least wide enough to type in.
fn artboard_name_field_rect(app: &VectorcraftApp, ui: &Ui, xf: &Xf, index: usize) -> Option<egui::Rect> {
    let st = app.session.active()?;
    let ab = st.doc.artboards.get(index)?;
    let artboard_tool = app.session.tool_id() == "artboard";
    let tool_labelled = artboard_tool.then(|| app.session.tool_options()["active"].as_u64()).flatten();
    let painter = ui.painter();
    let rect = if tool_labelled == u64::try_from(index).ok() {
        tool_name_rect(painter, xf, ab.rect, index, &ab.name)
    } else {
        let corner = xf.to_screen(Point::new(ab.rect.x0, ab.rect.y0));
        let width = (ab.rect.width() * xf.zoom) as f32;
        canvas_name_rect(painter, corner, width, &format!("{:02} - {}", index + 1, ab.name))?
    };
    Some(egui::Rect::from_min_size(rect.min, vec2(rect.width().max(80.0), rect.height().max(16.0))))
}

/// Is overlay label `text` the Artboard tool's "01 - <artboard name>"? It holds a name, so it is
/// shown as it is; the tools' other labels ("anchor", "path", Puppet Warp's warning) are ours.
fn names_an_artboard(text: &str) -> bool {
    text.split_once(" - ").is_some_and(|(n, _)| n.len() >= 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn draw_overlays(p: &egui::Painter, xf: &Xf, overlays: &[Overlay], t: &Tokens, look: HandleLook) {
    for o in overlays {
        match o {
            // Where the tools select: the document rectangle, turned with the view.
            Overlay::Marquee(r) => marquee_through(p, &xf.quad(*r)),
            Overlay::Path { path, color, width, dashed } => {
                let s = Stroke::new(*width, c32(*color));
                if *dashed {
                    for line in to_screen_path(path, xf) {
                        p.extend(Shape::dashed_line(&line, s, 4.0, 3.0));
                    }
                } else {
                    stroke_path(p, path, xf, s);
                }
            }
            Overlay::Line { a, b, color, dashed } => {
                let s = Stroke::new(1.0, c32(*color));
                let (a, b) = (xf.to_screen(*a), xf.to_screen(*b));
                if *dashed {
                    p.extend(Shape::dashed_line(&[a, b], s, 4.0, 3.0));
                } else {
                    p.line_segment([a, b], s);
                }
            }
            Overlay::Anchor { p: pt, color, filled, size } => anchor_square(p, xf.to_screen(*pt), c32(*color), *filled, *size),
            Overlay::CurvePoint { p: pt, color, filled, size } => curve_point(p, xf.to_screen(*pt), c32(*color), *filled, *size + look.grow),
            Overlay::Handle { p: pt, color } => look.draw(p, xf.to_screen(*pt), c32(*color)),
            Overlay::Label { p: pt, text, color } => {
                let sp = xf.to_screen(*pt) + vec2(8.0, -14.0);
                p.text(
                    sp,
                    egui::Align2::LEFT_TOP,
                    crate::panels::label_or_name(text, !names_an_artboard(text)),
                    egui::FontId::proportional(11.0),
                    c32(*color),
                );
            }
            Overlay::Highlight { quad, color } => {
                let c = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                p.add(Shape::convex_polygon(quad.iter().map(|q| xf.to_screen(*q)).collect(), c, Stroke::NONE));
            }
            Overlay::Measure { p: pt, text } => {
                let sp = xf.to_screen(*pt) + vec2(14.0, 14.0);
                let galley = p.layout(text.clone(), egui::FontId::proportional(11.0), Color32::WHITE, 200.0);
                let r = egui::Rect::from_min_size(sp, galley.size() + vec2(12.0, 8.0));
                p.rect_filled(r, CornerRadius::same(3), t.measure_bg);
                p.galley(sp + vec2(6.0, 4.0), galley, Color32::WHITE);
            }
            Overlay::Distance { a, b, text, color } => distance_guide(p, xf.to_screen(*a), xf.to_screen(*b), text, c32(*color)),
            Overlay::GridLine { a, b, color } => {
                let c = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                p.line_segment([xf.to_screen(*a), xf.to_screen(*b)], Stroke::new(1.0, c));
            }
            Overlay::Swatch { p: pt, color, selected } => {
                // A white disc under the colour shows its opacity; a dark rim keeps it readable on
                // any art, and the accent ring marks the selected stop.
                let c = xf.to_screen(*pt);
                p.circle_filled(c, 6.0, Color32::WHITE);
                p.circle_filled(c, 5.0, Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]));
                p.circle_stroke(c, 6.0, Stroke::new(1.0, Color32::from_gray(32)));
                if *selected {
                    p.circle_stroke(c, 8.0, Stroke::new(2.0, t.accent));
                }
            }
        }
    }
}

/// A Distance Guides measurement on screen ([`Overlay::Distance`]): the line from `a` to `b` in the
/// smart guide colour `color` with a 4 px tick across each end, and `text` in a pill of that colour
/// on its middle, in white or black, whichever reads on it.
fn distance_guide(p: &egui::Painter, a: Pos2, b: Pos2, text: &str, color: Color32) {
    let s = Stroke::new(1.0, color);
    p.line_segment([a, b], s);
    let along = b - a;
    let len = along.length();
    if len > 0.0 && len.is_finite() {
        let across = vec2(-along.y, along.x) / len * 4.0;
        for end in [a, b] {
            p.line_segment([end - across, end + across], s);
        }
    }
    let luma = 0.299 * f32::from(color.r()) + 0.587 * f32::from(color.g()) + 0.114 * f32::from(color.b());
    let ink = if luma > 150.0 { Color32::BLACK } else { Color32::WHITE };
    let galley = p.layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.0), ink);
    let r = egui::Rect::from_center_size(a + along / 2.0, galley.size() + vec2(8.0, 4.0));
    p.rect_filled(r, CornerRadius::same(3), color);
    p.galley(r.min + vec2(4.0, 2.0), galley, ink);
}

/// The Home screen (`app.home`; with no document open, unless its preference is off).
fn home(app: &mut VectorcraftApp, ui: &mut Ui, rect: egui::Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(rect, 0.0, t.panel_darker);
    // Taller than the window (a small window, many recent files): it scrolls. Widgets scrolled out
    // of view are clipped, so they take no pointer.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt("home-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        let side = (ui.available_width() - 820.0).max(40.0) / 2.0;
        let width = (ui.available_width() - 2.0 * side).max(0.0);
        ui.add_space(60.0);
        ui.horizontal_top(|ui| {
            ui.add_space(side);
            ui.allocate_ui_with_layout(vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(width);
                home_content(app, ui);
            });
        });
        ui.add_space(60.0);
    });
}

/// The Home screen's column: welcome, New / Open, Recent Files, presets and community links.
fn home_content(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("Welcome to VectorCraft")).font(theme::semibold(26.0)).color(t.text));
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Vector illustration — fast, open, scriptable.")).size(14.0).color(t.text_dim));
    ui.add_space(22.0);
    ui.horizontal(|ui| {
        if widgets::primary_button(ui, tl!("New file")).clicked() {
            app.run("file.newDialog", json!({})).ok();
        }
        ui.add_space(8.0);
        if widgets::secondary_button(ui, tl!("Open")).clicked() {
            app.run("file.open", json!({})).ok();
        }
    });
    recent(app, ui);
    ui.add_space(28.0);
    ui.label(egui::RichText::new(tl!("Quickly start a new file")).font(theme::semibold(14.0)).color(t.text));
    ui.add_space(10.0);
    // A few of New Document's presets (`file.newPresets`), as its cards.
    let presets = ["Letter", "A4", "Web 1920×1080", "Phone 390×844", "Postcard", "Social Square Post 1080×1080"];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
        let cards: Vec<_> = presets.iter().filter_map(|name| vectorcraft_engine::cmd::newdoc::find(&app.session, name)).collect();
        for s in cards {
            if crate::dialogs::preset_card(ui, &s, false).clicked() {
                app.run("file.new", json!({ "preset": s.name })).ok();
            }
        }
    });
    ui.add_space(28.0);
    ui.label(egui::RichText::new(tl!("Community")).font(theme::semibold(14.0)).color(t.text));
    ui.add_space(10.0);
    crate::community::links(app, ui);
}

/// The Home screen's Recent Files (#663): the first of File › Open Recent Files, each its name and
/// folder; a click opens it. Nothing when there are none.
fn recent(app: &mut VectorcraftApp, ui: &mut Ui) {
    const SHOWN: usize = 6;
    let files: Vec<String> = crate::io::recent_files(app).iter().take(SHOWN).cloned().collect();
    if files.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.add_space(28.0);
    ui.label(egui::RichText::new(tl!("Recent Files")).font(theme::semibold(14.0)).color(t.text));
    ui.add_space(8.0);
    for path in files {
        if recent_row(ui, &path).clicked() {
            // A failure is reported to the user there.
            let _ = crate::io::open_recent_reporting(app, &path);
        }
    }
}

/// One Recent Files row across the Home screen: history icon, name (accent) and folder (dim,
/// truncated). The whole row is one button that lights up under the pointer; its tooltip is the
/// full path.
fn recent_row(ui: &mut Ui, path: &str) -> egui::Response {
    const PAD: f32 = 6.0;
    const ICON: f32 = 16.0;
    let t = Tokens::get(ui.ctx());
    let p = std::path::Path::new(path);
    let name = p.file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let folder = p.parent().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), t.hover);
        }
        let icon = egui::Rect::from_center_size(egui::pos2(rect.left() + PAD + ICON / 2.0, rect.center().y), vec2(ICON, ICON));
        crate::icons::paint(ui, "history", icon, t.icon);
        let x = icon.right() + 8.0;
        let name_galley = egui::WidgetText::from(egui::RichText::new(&name).size(13.0).color(t.accent)).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (rect.right() - PAD - x).max(0.0),
            egui::TextStyle::Body,
        );
        let folder_x = x + name_galley.size().x + 10.0;
        ui.painter().galley(egui::pos2(x, rect.center().y - name_galley.size().y / 2.0), name_galley, t.accent);
        let room = rect.right() - PAD - folder_x;
        if room > 0.0 && !folder.is_empty() {
            let folder_galley = egui::WidgetText::from(egui::RichText::new(&folder).size(12.0).color(t.text_dim)).into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                room,
                egui::TextStyle::Body,
            );
            ui.painter().galley(egui::pos2(folder_x, rect.center().y - folder_galley.size().y / 2.0), folder_galley, t.text_dim);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    resp.on_hover_text(path).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn kurbo_flatten(p: &BezPath, tol: f64, f: &mut impl FnMut(PathEl)) {
    // Bounded per curve: a document's far-out handles, zoomed onto the screen, made kurbo
    // reserve gigabytes for the selection outline.
    vectorcraft_geom::flatten(p.elements().iter().copied(), tol, f);
}

/// The rendered raster `img` as an egui image. Both are premultiplied RGBA8, so its bytes are copied
/// over whole instead of pixel by pixel, which costs milliseconds for a canvas on a large screen.
/// A raster whose bytes don't match its size gives an empty image of that size.
pub(crate) fn color_image(img: &vectorcraft_render::Rendered) -> egui::ColorImage {
    let size = [img.width as usize, img.height as usize];
    let mut pixels = vec![egui::Color32::TRANSPARENT; size[0] * size[1]];
    let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut pixels);
    if bytes.len() == img.pixels.len() {
        bytes.copy_from_slice(&img.pixels);
    }
    egui::ColorImage::new(size, pixels)
}

/// The pixels of the `w`×`h` frame through `view` to render again for `doc`, when only the document
/// changed, or placed documents' bitmaps were made, since the frame shown (its key `key` but for
/// the revision and [`CacheKey::placed`], and its render options `opts`), which this thread
/// rendered, and no frame is on its way from the worker: those it changed in, and those the
/// objects showing those bitmaps' files cover (`Some(None)`: none), grown to whole eighths of the
/// frame, as long as they are at most half of it. None: render the whole frame. Frames the worker
/// renders stay whole: its renderer keeps the rasters of the art's effects, this thread's doesn't.
fn changed_pixels(
    app: &VectorcraftApp,
    key: &CacheKey,
    doc: &vectorcraft_doc::Document,
    opts: &vectorcraft_render::RenderOptions,
    view: Affine,
    w: u32,
    h: u32,
) -> Option<Option<[u32; 4]>> {
    let (shown, _) = app.canvas.shown.as_ref().filter(|(_, o)| o == opts)?;
    // A viewed opacity mask shows its coverage wherever its art reaches, past its object's bounds.
    if opts.mask_view.is_some() {
        return None;
    }
    let shown_key = app.canvas.key.as_ref()?;
    let same_view = *shown_key == CacheKey { revision: shown_key.revision, placed: shown_key.placed, ..key.clone() };
    if !same_view || app.canvas.texture.is_none() || app.canvas.from_worker || app.canvas.worker.as_ref().is_some_and(|w| w.pending.is_some()) {
        return None;
    }
    // The placed documents' files whose bitmaps were made since the frame shown.
    let placed = if shown_key.placed == key.placed {
        vectorcraft_render::Changed::Nothing
    } else {
        vectorcraft_render::placed_area(doc, &vectorcraft_render::placed_document::made_between(shown_key.placed, key.placed)?)
    };
    match vectorcraft_render::changed(shown, doc).and(placed) {
        vectorcraft_render::Changed::Nothing => Some(None),
        vectorcraft_render::Changed::Area(area) => match vectorcraft_render::view_pixels(area, view, w, h) {
            None => Some(None),
            Some([x0, y0, x1, y1]) => {
                let ((x0, x1), (y0, y1)) = (in_steps(x0, x1, w), in_steps(y0, y1, h));
                (u64::from(x1 - x0) * u64::from(y1 - y0) * 2 <= u64::from(w) * u64::from(h)).then_some(Some([x0, y0, x1, y1]))
            }
        },
        vectorcraft_render::Changed::Everything => None,
    }
}

/// `a..b` of `0..n`, grown to whole eighths of `n` and moved back into `0..n`: the parts of a
/// frame rendered again come in few sizes, which keep the contexts the renderer keeps.
fn in_steps(a: u32, b: u32, n: u32) -> (u32, u32) {
    let step = n.div_ceil(8).max(1);
    let len = (b - a).max(1).div_ceil(step).saturating_mul(step).min(n);
    let a = a.min(n - len);
    (a, a + len)
}

/// Upload the art's raster; Pixel Preview's document pixels keep hard edges when magnified.
fn upload(app: &mut VectorcraftApp, ctx: &egui::Context, img: &vectorcraft_render::Rendered, pixel: bool) {
    let color = color_image(img);
    let options = if pixel { egui::TextureOptions::NEAREST } else { egui::TextureOptions::LINEAR };
    match &mut app.canvas.texture {
        Some(tex) => tex.set(color, options),
        None => app.canvas.texture = Some(ctx.load_texture("canvas", color, options)),
    }
}

/// Pixel Preview: the whole document pixels (x0, y0, x1, y1) under the canvas, rotated view
/// included. None when the view is degenerate.
fn pixel_region(xf: &Xf) -> Option<[i64; 4]> {
    let r = xf.rect;
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|p| xf.to_doc(p));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in corners {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let (x0, y0, x1, y1) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
    // A document pixel is bigger than a screen pixel here, so the region is about canvas-sized.
    let ok = [x0, y0, x1, y1].iter().all(|c| c.is_finite() && c.abs() < 1e9) && x1 > x0 && y1 > y0 && x1 - x0 <= 16384.0 && y1 - y0 <= 16384.0;
    ok.then_some([x0 as i64, y0 as i64, x1 as i64, y1 as i64])
}

/// The Contextual Task Bar's area.
fn task_bar_id() -> egui::Id {
    egui::Id::unique("task-bar")
}

/// Where the Contextual Task Bar was last drawn (none while it isn't shown).
pub(crate) fn task_bar_rect(ctx: &egui::Context) -> Option<egui::Rect> {
    let id = task_bar_id();
    ctx.memory(|m| m.areas().is_visible(&egui::LayerId::new(egui::Order::Middle, id)).then(|| m.area_rect(id)).flatten())
}

/// Whether the objects `ids` (or anything inside them) are painted with a colour: a solid colour,
/// a gradient's stops or a mesh's points, which Recolor Artwork can change.
pub(crate) fn has_colors(doc: &vectorcraft_doc::Document, ids: &[vectorcraft_doc::NodeId]) -> bool {
    ids.iter().filter_map(|id| doc.node(*id)).any(|n| {
        let mut found = false;
        vectorcraft_doc::swatches::node_colors(n, &mut |_, _| found = true);
        found
    })
}

/// The bounds of the artboards the Artboard tool has selected ([`crate::panels::artboards::picked`]).
fn picked_artboards_bounds(app: &VectorcraftApp) -> Option<Rect> {
    let st = app.session.active()?;
    let picked = crate::panels::artboards::picked(app, st.doc.artboards.len());
    picked.iter().filter_map(|i| st.doc.artboards.get(*i)).map(|a| a.rect).reduce(|a, b| a.union(b))
}

/// The Contextual Task Bar: a floating pill under the selection with the most likely next actions.
/// With the Artboard tool it sits under the selected artboards and offers Resize (Artboard
/// Options), Duplicate, Rearrange All (with two or more artboards) and Export (Export for Screens).
/// Its handle drags it anywhere on the canvas; unpinned it keeps that offset as it follows the
/// selection, pinned it stays put ([`crate::state::TaskBarPlace`]).
fn task_bar(app: &mut VectorcraftApp, ui: &mut Ui, xf: &Xf) {
    let t = Tokens::get(ui.ctx());
    let on_artboards = app.session.tool_id() == "artboard";
    if !on_artboards
        && (app.session.active().is_none_or(|st| st.selection.is_empty())
            || !matches!(app.session.tool_id(), "selection" | "directSelection" | "groupSelection"))
    {
        return;
    }
    let Some(b) = (if on_artboards { picked_artboards_bounds(app) } else { app.selection_bounds() }) else { return };
    let Some(st) = app.session.active() else { return };
    let n = st.selection.len();
    let first = st.selection.objects.first().and_then(|id| st.doc.node(*id)).cloned();
    let is_group = first.as_ref().is_some_and(|f| matches!(f.kind, NodeKind::Group { .. }));
    let is_image = first.as_ref().is_some_and(|f| matches!(f.kind, NodeKind::Image(_)));
    let is_clip_group = first.as_ref().is_some_and(|f| matches!(f.kind, NodeKind::Group { clip: true, .. }));
    let is_compound = first.as_ref().is_some_and(|f| matches!(f.kind, NodeKind::Compound { .. }));
    let open_path = first.as_ref().is_some_and(|f| matches!(&f.kind, NodeKind::Path { path, .. } if !path.is_closed()));
    let text = first.as_ref().and_then(|f| match &f.kind {
        NodeKind::Text(t) => Some(&t.kind),
        _ => None,
    });
    // Images and vector art selected together: a clipping mask is the likely next step.
    let mixed = n > 1 && {
        let images = st.selection.objects.iter().filter_map(|id| st.doc.node(*id)).filter(|o| matches!(o.kind, NodeKind::Image(_))).count();
        images > 0 && images < n
    };
    // An Image Trace object: the group the engine names "Image Trace", its source image first.
    let is_trace = is_group
        && first.as_ref().is_some_and(|f| {
            f.name.as_deref() == Some("Image Trace") && f.children().is_some_and(|c| c.first().is_some_and(|i| matches!(i.kind, NodeKind::Image(_))))
        });
    let mut items: Vec<(&str, &str, &str, serde_json::Value)> = vec![]; // (label, icon, command, params)
    let none = || json!({});
    if on_artboards {
        // As in the reference app, the Artboard tool offers to resize, duplicate and export the
        // artboards (<https://community.adobe.com/questions-652/contextual-task-bar-in-illustrator-2025-quick-actions-when-you-need-them-1546580>),
        // and to rearrange them (<https://creativepro.com/managing-artboards-in-illustrator/>).
        items.push((tl!("Resize"), "move-diagonal-2", TASK_BAR_ARTBOARD_OPTIONS, none()));
        items.push((tl!("Duplicate"), "copy", TASK_BAR_DUPLICATE_ARTBOARDS, none()));
        if st.doc.artboards.len() > 1 {
            items.push((tl!("Rearrange All"), "dc-rearrange", TASK_BAR_REARRANGE_ARTBOARDS, none()));
        }
        items.push((tl!("Export"), "share-2", "file.exportForScreens", none()));
    } else {
        // Edit Path: the Direct Selection tool, to edit the path's anchors.
        let edit_path = || (tl!("Edit Path"), "dc-direct", "tool.select", json!({"tool": "directSelection"}));
        // As in the reference app, an image offers Image Trace and Crop Image, and traced art offers
        // Expand (<https://community.adobe.com/announcements-651/contextual-task-bar-in-illustrator-2025-quick-actions-when-you-need-them-1546580>).
        if n == 1 && is_image {
            items.push((tl!("Image Trace"), "image", TASK_BAR_IMAGE_TRACE, none()));
            items.push((tl!("Crop Image"), "dc-pf-crop", "ui.cropImage", none()));
        }
        // Direct-selected anchors take the place of a path's Offset Path and Simplify. For them the bar
        // offers Remove, Connect, Cut and Convert to corner or smooth, as the reference app does
        // (<https://helpx.adobe.com/illustrator/desktop/get-started/learn-the-basics/contextual-task-bar-overview.html>).
        // These run the same commands as the Control bar's anchor buttons.
        let anchors = !st.selection.anchors.is_empty();
        if anchors {
            items.push((tl!("Remove Anchor Points"), "pen-tool-delete", "path.removeAnchors", none()));
            items.push((tl!("Connect"), "dc-join", "path.join", none()));
            items.push((tl!("Cut Path"), "scissors", "path.cutAtAnchors", none()));
            items.push((tl!("Corner"), "dc-anchor", TASK_BAR_TO_CORNER, none()));
            items.push((tl!("Smooth"), "dc-anchor-smooth", TASK_BAR_TO_SMOOTH, none()));
        }
        // As in the reference app, open paths offer Join and Edit Path, compound paths and clipping
        // masks their Release, images selected with vector art Make Clipping Mask, and type converts
        // between point and area type (<https://helpx.adobe.com/illustrator/desktop/get-started/learn-the-basics/contextual-task-bar-overview.html>).
        if n > 1 {
            items.push((tl!("Group"), "group", "object.group", none()));
            if mixed {
                items.push((tl!("Make Clipping Mask"), "dc-pf-crop", "object.clippingMask.make", none()));
            } else {
                items.push((tl!("Unite"), "squares-unite", "object.pathfinder.unite", none()));
            }
        } else if is_trace {
            items.push((tl!("Expand"), "maximize-2", "imageTrace.expand", none()));
        } else if is_clip_group {
            items.push((tl!("Edit Clipping Path"), "dc-direct", "object.clippingMask.editMask", none()));
            items.push((tl!("Release Clipping Mask"), "ungroup", "object.clippingMask.release", none()));
        } else if is_group {
            items.push((tl!("Ungroup"), "ungroup", "object.ungroup", none()));
            items.push((tl!("Isolate"), "square-dashed", "object.isolate", none()));
        } else if is_compound {
            items.push(edit_path());
            items.push((tl!("Release Compound Path"), "ungroup", "object.compoundPath.release", none()));
        } else if let Some(kind) = text {
            items.push((tl!("Create Outlines"), "type", "type.createOutlines", none()));
            // Point type converts to area type and back; type on a path doesn't.
            match kind {
                TextKind::Area { .. } => items.push((tl!("Convert To Point Type"), "type", "type.convertToPointType", none())),
                TextKind::Point => items.push((tl!("Convert To Area Type"), "dc-type-area", "type.convertToAreaType", none())),
                TextKind::OnPath { .. } => {}
            }
        } else if is_image {
            // Offset Path and Simplify don't apply to an image.
        } else if !anchors {
            if open_path {
                items.push((tl!("Join"), "dc-join", "path.join", none()));
            }
            items.push(edit_path());
            items.push((tl!("Offset Path"), "square-dashed", "object.path.offsetPath", none()));
            items.push((tl!("Simplify"), "spline", "object.path.simplify", none()));
        }
        items.push((tl!("Duplicate"), "copy", "edit.duplicate", none()));
    }
    let fill = first.as_ref().map(|f| f.appearance.fill_paint()).unwrap_or_default();
    let colored = has_colors(&st.doc, &st.selection.objects);
    let doc = st.uid;
    // Under the selection's lowest corner on screen, centered on the selection (the view may be
    // rotated).
    let lowest = xf.quad(b).iter().map(|c| c.y).fold(f32::NEG_INFINITY, f32::max);
    let anchor = pos2(xf.to_screen(b.center()).x, lowest);
    let est_w = 118.0 + items.iter().map(|(l, _, _, _)| l.len() as f32 * 7.2 + 44.0).sum::<f32>();
    // Its place under the selection, then where the handle moved it, kept on the canvas (with the
    // bar's size last frame, the estimate before it first shows).
    let under = pos2(anchor.x - est_w / 2.0, anchor.y + 28.0);
    let id = task_bar_id();
    let size = egui::AreaState::load(ui.ctx(), id).and_then(|s| s.size).unwrap_or(vec2(est_w, 44.0));
    let bounds = xf.rect.shrink(8.0);
    let keep_in = |p: Pos2| p.clamp(bounds.min, (bounds.max - size).max(bounds.min));
    let place = &mut app.ui.task_bar_place;
    // Just unpinned: follow the selection from the pinned spot.
    if !place.pinned
        && let Some(at) = place.pin_at.take()
    {
        place.offset = Some((doc, xf.rect.min + at - under));
    }
    let pos = keep_in(match place.pin_at {
        Some(at) => xf.rect.min + at,
        None => under + place.offset.filter(|(d, _)| *d == doc).map_or(Vec2::ZERO, |(_, o)| o),
    });
    place.shown_at = Some(pos - xf.rect.min);
    // Pinned before it ever showed: hold it here.
    if place.pinned && place.pin_at.is_none() {
        place.pin_at = place.shown_at;
    }
    let pinned = place.pinned;
    let mut moved = Vec2::ZERO;
    let mut run: Option<(&str, serde_json::Value)> = None;
    egui::Area::new(id).order(egui::Order::Middle).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::NONE
            .fill(t.panel)
            .stroke(Stroke::new(1.0, t.tool_active))
            .corner_radius(CornerRadius::same(5))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .shadow(egui::epaint::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(70) })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (g, grip) = ui.allocate_exact_size(vec2(4.0, 28.0), Sense::drag());
                    let active = grip.hovered() || grip.dragged();
                    ui.painter().rect_filled(g.shrink2(vec2(0.5, 4.0)), CornerRadius::same(2), if active { t.text_dim } else { t.button_border });
                    if grip.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        moved = grip.drag_delta();
                    } else if grip.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    for (label, icon, cmd, params) in &items {
                        let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), t.text_strong);
                        let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 38.0, 30.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(3), t.hover);
                        }
                        ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.button_border), StrokeKind::Inside);
                        crate::icons::paint(ui, icon, egui::Rect::from_min_size(r.min + vec2(8.0, 7.0), vec2(16.0, 16.0)), t.icon);
                        ui.painter().galley(pos2(r.left() + 30.0, r.center().y - galley.size().y / 2.0), galley, t.text_strong);
                        if resp.clicked() {
                            run = Some((*cmd, params.clone()));
                        }
                    }
                    // The selected objects' Fill, Recolor Artwork and Lock, left out with the Artboard tool.
                    if !on_artboards {
                        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 30.0), Sense::click());
                        widgets::paint_chip(ui, egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0)), &fill);
                        ui.painter().rect_stroke(
                            egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0)),
                            0.0,
                            Stroke::new(1.0, t.button_border),
                            StrokeKind::Outside,
                        );
                        if resp.on_hover_text(tl!("Fill")).clicked() {
                            app.session.fill_active = true;
                            app.ui.open_panel = Some("swatches".into());
                        }
                        // Recolor Artwork, when the selection has colours to recolour.
                        if colored && widgets::icon_button(ui, "palette", tl!("Recolor Artwork"), false, 30.0).clicked() {
                            run = Some(("ui.recolorDialog", json!({})));
                        }
                        if widgets::icon_button(ui, "lock", tl!("Lock (⌘2)"), false, 30.0).clicked() {
                            run = Some(("object.lock", json!({})));
                        }
                    }
                    let more = widgets::icon_button(ui, "ellipsis", tl!("More Options"), false, 30.0);
                    egui::Popup::menu(&more).show(|ui| {
                        ui.set_min_width(170.0);
                        let bar = [
                            (tl!("Hide Bar"), "window.taskBar", false),
                            (tl!("Pin Bar Position"), "window.taskBar.pin", pinned),
                            (tl!("Reset Bar Position"), "window.taskBar.reset", false),
                        ];
                        for (label, cmd, checked) in bar {
                            if widgets::menu_item(ui, label, true, checked) {
                                run = Some((cmd, json!({})));
                            }
                        }
                        ui.separator();
                        if widgets::menu_item(ui, tl!("Show Properties Panel"), true, false) {
                            run = Some(("window.panel", json!({"panel": "properties"})));
                        }
                    });
                });
            });
    });
    if moved != Vec2::ZERO {
        let to = keep_in(pos + moved);
        let place = &mut app.ui.task_bar_place;
        if place.pinned {
            place.pin_at = Some(to - xf.rect.min);
        } else {
            place.offset = Some((doc, to - under));
        }
        ui.ctx().request_repaint();
    }
    match run {
        // Image Trace traces the image at once with the Default preset. As a quick trace from
        // Properties or the Control bar, it leaves the Image Trace panel as it was.
        Some((TASK_BAR_IMAGE_TRACE, _)) => crate::menus::invoke(app, "imageTrace.make", json!({"preset": "Default"})),
        // The Artboard tool's buttons: Artboard Options for the active artboard, as Properties'
        // icon opens it; Duplicate for every selected artboard; Rearrange All Artboards.
        Some((TASK_BAR_ARTBOARD_OPTIONS, _)) => crate::menus::invoke(app, "tool.options", json!({"tool": "artboard"})),
        Some((TASK_BAR_DUPLICATE_ARTBOARDS, _)) => crate::panels::artboards::duplicate_picked(app),
        Some((TASK_BAR_REARRANGE_ARTBOARDS, _)) => crate::panels::artboards::open_rearrange(app),
        // Corner and Smooth convert the direct-selected anchors, as the Control bar's buttons do.
        Some((TASK_BAR_TO_CORNER, _)) => crate::menus::invoke(app, "path.convertAnchors", json!({"to": "corner"})),
        Some((TASK_BAR_TO_SMOOTH, _)) => crate::menus::invoke(app, "path.convertAnchors", json!({"to": "smooth"})),
        Some((c, p)) => crate::menus::invoke(app, c, p),
        None => {}
    }
}

/// The Contextual Task Bar's Image Trace button: a trace with the Default preset.
const TASK_BAR_IMAGE_TRACE: &str = "taskBar.imageTrace";
/// The Contextual Task Bar's Resize button with the Artboard tool: Artboard Options.
const TASK_BAR_ARTBOARD_OPTIONS: &str = "taskBar.artboardOptions";
/// The Contextual Task Bar's Duplicate button with the Artboard tool.
const TASK_BAR_DUPLICATE_ARTBOARDS: &str = "taskBar.duplicateArtboards";
/// The Contextual Task Bar's Rearrange All button with the Artboard tool.
const TASK_BAR_REARRANGE_ARTBOARDS: &str = "taskBar.rearrangeArtboards";
/// The Contextual Task Bar's Corner button for direct-selected anchors.
const TASK_BAR_TO_CORNER: &str = "taskBar.toCorner";
/// The Contextual Task Bar's Smooth button for direct-selected anchors.
const TASK_BAR_TO_SMOOTH: &str = "taskBar.toSmooth";

#[cfg(test)]
mod tests;
