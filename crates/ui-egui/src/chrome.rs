//! Window chrome: application bar (menus), Control bar, document tabs, status bar.

use std::borrow::Cow;
use std::sync::Arc;

use egui::{CornerRadius, Sense, Stroke, Ui, vec2};
use serde_json::json;
use vectorcraft_doc::NodeKind;

use crate::panels::stroke as stroke_panel;
use crate::state::{ROTATE_VIEW_ANGLES, ZOOM_STOPS, zoom_label};
use crate::theme::{self, Tokens};
use crate::widgets;
use crate::{VectorcraftApp, icons, menus, titlebar};

/// The application bar's height, in points (compact, as in PhotoCraft).
pub const APP_BAR_HEIGHT: f32 = 32.0;
/// The brand mark's side.
const MARK: f32 = 18.0;
/// Height of the workspace switcher.
const WIDGET_H: f32 = 22.0;

/// The application bar: brand mark, Home, menus, then Discord, search and the workspace switcher
/// at the right. With [`VectorcraftApp::custom_titlebar`] it is also the window's title bar
/// ([`titlebar`]): the caption buttons take the right end and the rest of the bar drags the window.
/// On macOS, under the system's transparent title bar, its empty space drags and zooms the window
/// too ([`titlebar::drag_area`]). With the system title bar (Windows and Linux, `system_title_bar`) the OS draws its own icon and
/// title, so the in-app bar shows Home, menus and workspace controls only: no brand mark.
pub fn app_bar(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let custom = app.custom_titlebar;
    let system = system_title_bar(app);
    let left = app.titlebar_inset.clamp(8.0, f32::from(i8::MAX)) as i8;
    let frame = egui::Frame::NONE.fill(t.app_bar).inner_margin(egui::Margin { left, right: if custom { 0 } else { 14 }, top: 0, bottom: 0 });
    let bar = egui::Panel::top("app_bar").exact_size(APP_BAR_HEIGHT).frame(frame.stroke(Stroke::new(1.0, t.border))).show(ui, |ui| {
        // macOS draws the bar under its transparent title bar, so the bar gets the title bar's
        // clicks: without this a double-click there zooms nothing (#1932).
        if custom || cfg!(target_os = "macos") {
            titlebar::drag_area(ui, ui.max_rect());
        }
        ui.horizontal_centered(|ui| {
            // With the system title bar the OS draws its own icon: no in-app brand mark.
            if !system {
                let (r, _) = ui.allocate_exact_size(vec2(MARK, MARK), Sense::hover());
                crate::brand::paint_mark(ui, r);
                ui.add_space(6.0);
            }
            let on_home = menus::home_showing(app);
            if widgets::icon_button(ui, "house", tl!("Home"), on_home, 20.0).clicked() {
                app.run("app.home", json!({})).ok();
            }
            ui.add_space(2.0);
            // With the macOS menu bar the menus are at the top of the screen instead.
            let menus_end = if app.services.native_menu.is_some() { ui.cursor().min.x } else { menus::menu_bar(app, ui) };
            // The right-side group fills the space after the menus from the right; when it runs
            // short, Discord goes first (it is also under Help), then the search icon (the palette
            // stays under its shortcut), then the workspace switcher narrows.
            let full = ui.max_rect();
            let right_edge = if custom { full.right() - titlebar::WIDTH - 10.0 } else { full.right() };
            let right = egui::Rect::from_min_max(egui::pos2(menus_end + 8.0, full.top()), egui::pos2(right_edge, full.bottom()));
            let room = right.width();
            // Built-in workspace names translate; the user's own stay as typed.
            let ws_name =
                if crate::workspaces::is_builtin(&app.ui.workspace) { tl!(&app.ui.workspace).to_string() } else { app.ui.workspace.clone() };
            let ws = ui.painter().layout_no_wrap(ws_name, egui::FontId::proportional(12.0), t.text);
            let ws_w = (ws.size().x + 36.0).clamp(112.0, 190.0);
            let gap = ui.spacing().item_spacing.x;
            // The search icon, as in PhotoCraft: shown while a minimal switcher fits beside it.
            let icon = 28.0;
            let search = room >= 64.0 + 8.0 + gap + icon;
            let ws_w = ws_w.min(room - 8.0 - gap - if search { icon } else { 0.0 }).max(64.0);
            let discord = search && room >= ws_w + 8.0 + gap + icon + 10.0 + gap + crate::community::discord_width(ui, false);
            let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::right_to_left(egui::Align::Center)));
            let ui = &mut rui;
            // Workspace switcher: shows the current workspace, opens Window → Workspace.
            let (wr, wresp) = ui.allocate_exact_size(vec2(ws_w, WIDGET_H), Sense::click());
            ui.painter().rect_filled(wr, CornerRadius::same(4), if wresp.hovered() { t.hover } else { t.panel });
            ui.painter().with_clip_rect(wr.shrink2(vec2(4.0, 0.0))).galley(wr.left_center() + vec2(10.0, -ws.size().y / 2.0), ws, t.text);
            icons::paint(ui, "chevron-down", egui::Rect::from_center_size(wr.right_center() - vec2(12.0, 0.0), vec2(12.0, 12.0)), t.text_dim);
            let wresp = wresp.on_hover_text(tl!("Switch workspace"));
            egui::Popup::menu(&wresp).show(|ui| crate::workspaces::popup(app, ui));
            ui.add_space(8.0);
            // Search → command palette (its shortcut opens it too).
            if search {
                let tip = match menus::shortcut_of("help.commandPalette") {
                    Some(sc) => format!("{}  ({})", tl!("Search commands and tools"), menus::pretty_shortcut(sc)),
                    None => tl!("Search commands and tools").to_string(),
                };
                if widgets::icon_button(ui, "search", &tip, app.ui.palette_open, icon).clicked() {
                    app.ui.palette_open = !app.ui.palette_open;
                    app.ui.palette_query.clear();
                }
            }
            if discord {
                ui.add_space(10.0);
                crate::community::discord_button(app, ui, false);
            }
            if app.services.native_menu.is_some() {
                let title = ui.painter().layout_no_wrap("VectorCraft".into(), egui::FontId::proportional(13.5), t.text);
                let title_rect = egui::Rect::from_center_size(full.center(), title.size());
                // Keep the title centered only while the interactive groups leave room for it.
                if title_rect.left() >= menus_end + 8.0 && title_rect.right() <= ui.min_rect().left() - 8.0 {
                    ui.painter().galley(title_rect.min, title, t.text);
                }
            }
        });
    });
    if custom {
        titlebar::caption_buttons(app, ui, bar.response.rect);
    }
}

/// Whether the window shows the system's title bar instead of the app drawing its own: Windows
/// and Linux with Preferences › User Interface › System Title Bar. macOS always has system
/// decorations, but keeps the in-app brand mark, so it never counts as system mode here; nor
/// does the web build (the browser tab has no document title), nor an embedder that never
/// turned the preference on.
pub fn system_title_bar(app: &VectorcraftApp) -> bool {
    !app.custom_titlebar && app.session.prefs.system_title_bar && !cfg!(any(target_os = "macos", target_arch = "wasm32"))
}

/// The OS window title: the active document's name suffixed with the app name (a `*` prefix
/// marks unsaved changes, like the `*` in the document tabs), or just the app name with no
/// document open.
pub fn window_title(app: &VectorcraftApp) -> String {
    app.session
        .active()
        .map(|d| format!("{}{} \u{2014} VectorCraft", if d.is_dirty() { "*" } else { "" }, d.title()))
        .unwrap_or_else(|| "VectorCraft".into())
}

/// Keep the OS window title (and the taskbar / Alt-Tab entry) on the active file. Sends
/// `ViewportCommand::Title` only when the title changed since the last frame.
pub fn sync_window_title(app: &mut VectorcraftApp, ctx: &egui::Context) {
    let want = window_title(app);
    if app.last_window_title != want {
        app.last_window_title = want.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(want));
        // One more frame: a new title makes AppKit lay its title bar out again, putting the window
        // buttons back where it keeps them until the next frame centres them on the bar (#968).
        ctx.request_repaint();
    }
}

/// An anchor button: icon, tooltip, command and the convert command's `to`.
type AnchorButton<'a> = (&'a str, &'a str, &'a str, Option<&'a str>);

/// The anchor controls the Control bar and the Properties panel show.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnchorControls {
    None,
    /// Paths selected as a whole with a tool that edits anchors (Direct Selection, the Pen…),
    /// which shows all their anchors selected: "Convert:" only, as removing or cutting at every
    /// anchor isn't what they're for.
    Convert,
    /// Anchors direct-selected: "Convert:" and "Anchors:".
    All,
}

/// Which anchor controls show for the selection and the active tool.
pub fn anchor_controls(app: &VectorcraftApp) -> AnchorControls {
    let Some(st) = app.session.active() else {
        return AnchorControls::None;
    };
    if !st.selection.anchors.is_empty() {
        return AnchorControls::All;
    }
    let paths = st.selection.objects.iter().any(|id| st.doc.node(*id).is_some_and(|n| matches!(n.kind, NodeKind::Path { .. })));
    if paths && vectorcraft_tools::catalog::edits_anchors(app.session.tool_id()) { AnchorControls::Convert } else { AnchorControls::None }
}

/// What the Control bar and the Properties panel offer for selected anchors ([`anchor_controls`]):
/// "Convert:" corner or smooth, then "Anchors:" remove, connect (Join) and cut. Each group is a
/// row of its own: inline in the Control bar, one under the other in a panel.
pub fn anchor_buttons(app: &mut VectorcraftApp, ui: &mut Ui, controls: AnchorControls) {
    let t = Tokens::get(ui.ctx());
    let mut run = None;
    let groups: [(&str, &[AnchorButton]); 2] = [
        (
            tl!("Convert:"),
            &[
                ("dc-anchor", tl!("Convert Selected Anchor Points to Corner"), "path.convertAnchors", Some("corner")),
                ("dc-anchor-smooth", tl!("Convert Selected Anchor Points to Smooth"), "path.convertAnchors", Some("smooth")),
            ],
        ),
        (
            tl!("Anchors:"),
            &[
                ("pen-tool-delete", tl!("Remove Anchor Points"), "path.removeAnchors", None),
                ("dc-join", tl!("Connect Selected End Points"), "path.join", None),
                ("scissors", tl!("Cut Path at Selected Anchor Points"), "path.cutAtAnchors", None),
            ],
        ),
    ];
    let shown = match controls {
        AnchorControls::None => 0,
        AnchorControls::Convert => 1,
        AnchorControls::All => groups.len(),
    };
    for (label, buttons) in groups.into_iter().take(shown) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).size(12.0).color(t.text));
            for &(icon, tip, id, to) in buttons {
                if widgets::icon_button(ui, icon, tip, false, 24.0).clicked() {
                    run = Some((id, to.map_or_else(|| json!({}), |to| json!({ "to": to }))));
                }
            }
        });
    }
    if let Some((id, p)) = run {
        crate::menus::invoke(app, id, p);
    }
}

/// The id of the Control bar's isolation button.
const ISOLATION_BUTTON: &str = "cb-isolation";

/// The room the Control bar needs for the Snapping menu next to the Artboard tool's controls; with
/// less, the menu is left out so those controls still fit the narrowest window.
const ARTBOARD_SNAPPING_WIDTH: f32 = 900.0;

/// The Control bar's snapping quick access; every change uses its existing command.
fn snapping_menu(app: &mut VectorcraftApp, ui: &mut Ui) {
    ui.menu_button(tl!("Snapping"), |ui| {
        for (label, cmd, on) in [
            (tl!("Smart Guides"), "view.smartGuides", app.ui.view.smart_guides),
            (tl!("Snap to Point"), "view.snapToPoint", app.ui.view.snap_to_point),
            (tl!("Snap to Grid"), "view.snapToGrid", app.ui.view.snap_to_grid),
            (tl!("Snap to Pixel"), "view.snapToPixel", app.ui.view.snap_to_pixel),
        ] {
            if widgets::check(ui, label, on, true) {
                crate::menus::invoke(app, cmd, json!({}));
            }
        }
        ui.separator();
        for (key, label, on) in [
            ("snapTangent", tl!("Snap to Tangent"), app.session.prefs.snap_tangent),
            ("snapPerpendicular", tl!("Snap to Perpendicular"), app.session.prefs.snap_perpendicular),
        ] {
            if widgets::check(ui, label, on, true) {
                crate::menus::invoke(app, "prefs.set", json!({"key": key, "value": !on}));
            }
        }
    });
}

/// The Control bar's isolation button. In isolation mode it is Exit Isolation Mode, except while a
/// pattern is edited (the isolation bar's Done and Cancel end that). Otherwise, with one object
/// selected that [`vectorcraft_tools::select::isolates`], it is Isolate Selected Object, which
/// also isolates a path inside a group that Direct Selection selected. An envelope's contents
/// get no button: the envelope's Edit Envelope and Edit Contents buttons show instead.
fn isolation_button(app: &mut VectorcraftApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else { return };
    let (icon, tip, id) = if st.isolation.is_some() {
        if st.doc.pattern_edit.is_some() {
            return;
        }
        ("chevron-left", tl!("Exit Isolation Mode"), "object.exitIsolation")
    } else if let [one] = st.selection.objects[..]
        && st.doc.node(one).is_some_and(|n| vectorcraft_tools::select::isolates(&n.kind))
        && vectorcraft_doc::live::envelope_of(&st.doc, one).is_none()
    {
        ("square-dashed", tl!("Isolate Selected Object"), "object.isolate")
    } else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    let resp = ui.interact(rect, egui::Id::unique(ISOLATION_BUTTON), Sense::click());
    if widgets::paint_icon_button(ui, resp, icon, tip, false).clicked() {
        crate::menus::invoke(app, id, json!({}));
    }
}

/// The room the Control bar's inline X/Y/W/H fields need; with less, only the Transform link shows.
const INLINE_TRANSFORM_WIDTH: f32 = 440.0;

/// The Control bar (Window → Control), context-sensitive like Illustrator's.
pub fn control_bar(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("control_bar")
        .exact_size(34.0)
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(10, 0)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                // Gripper
                let (g, _) = ui.allocate_exact_size(vec2(6.0, 20.0), Sense::hover());
                for i in 0..5 {
                    ui.painter().circle_filled(g.center_top() + vec2(0.0, 2.0 + i as f32 * 4.0), 0.9, t.text_disabled);
                }
                if app.session.active().is_none() {
                    ui.label(egui::RichText::new(tl!("No Document")).color(t.text_dim));
                    return;
                }
                // The Artboard tool's controls fill the narrowest window, so they keep it to themselves there.
                if app.session.tool_id() != "artboard" || ui.available_width() >= ARTBOARD_SNAPPING_WIDTH {
                    snapping_menu(app, ui);
                }
                let Some(st) = app.session.active() else { return };
                let sel = st.selection.objects.clone();
                let units = app.session.general_unit();
                let first = sel.first().and_then(|id| st.doc.node(*id)).cloned();
                let anchors = anchor_controls(app);
                let label = match &first {
                    _ if app.session.tool_id() == "artboard" => tl!("Artboard"),
                    Some(_) if sel.len() == 1 && anchors != AnchorControls::None => {
                        tl!("Anchor Point")
                    }
                    Some(vectorcraft_doc::Node { kind: NodeKind::Image(im), .. }) if sel.len() == 1 => {
                        if im.link.is_some() {
                            tl!("Linked File")
                        } else {
                            tl!("Embedded")
                        }
                    }
                    Some(n) if sel.len() == 1 && crate::panels::image_trace::is_trace(n) => {
                        tl!("Image Tracing")
                    }
                    _ => tl!(crate::panels::appearance::object_label(app)),
                };
                ui.label(egui::RichText::new(label).font(theme::semibold(12.0)).color(t.text));
                // With the Artboard tool in use, the bar shows the active artboard's controls in place
                // of the object controls.
                if crate::panels::artboards::control_bar(app, ui) {
                    return;
                }
                if anchors != AnchorControls::None {
                    anchor_buttons(app, ui, anchors);
                }
                isolation_button(app, ui);
                ui.add_space(6.0);
                // An image or an Image Trace object shows its own controls in place of Fill and Stroke.
                let image = crate::place::control_bar_details(app, ui) || crate::panels::image_trace::control_bar(app, ui);
                crate::toolbar::control_bar_options(app, ui);
                crate::dialogs::envelope::control_bar(app, ui);
                let opacity = if first.is_some() { crate::panels::current_transparency(app).map_or(1.0, |t| t.0) } else { 1.0 };
                if !image {
                    let shown_stroke = crate::panels::current_stroke(app);
                    let mixed = crate::panels::stroke_mixed(app, ui.ctx());
                    let weight = stroke_panel::shown_weight(app, shown_stroke.as_ref(), &mixed);
                    crate::panels::paint_chip(app, ui, false, 22.0, true);
                    crate::panels::paint_chip(app, ui, true, 22.0, true);
                    // The link opens the Stroke panel as a popover under it; then the weight spinner
                    // (with presets) and the width profile.
                    stroke_panel::link(app, ui, tl!("Stroke:"));
                    stroke_panel::weight_field(app, ui, "cb-stroke", weight, 100.0);
                    if let Some(id) = stroke_panel::profile_dropdown(app, ui, shown_stroke.as_ref().and_then(|s| s.profile.as_ref())) {
                        app.run("stroke.set", json!({"profile": id})).ok();
                    }
                    ui.add_space(4.0);
                    ui.separator();
                }
                if ui.link(egui::RichText::new(tl!("Opacity:")).size(12.0).color(t.text).underline()).clicked() {
                    app.ui.open_panel = Some("transparency".into());
                }
                if let Some(o) = widgets::plain_field(ui, "cb-opacity", opacity as f64 * 100.0, "%", 0, 56.0)
                    && !sel.is_empty()
                {
                    app.run("transparency.set", json!({"opacity": o.clamp(0.0, 100.0)})).ok();
                }
                if !sel.is_empty() {
                    // Style picker: the selection's graphic style; its menu applies another.
                    ui.add_space(4.0);
                    if ui.link(egui::RichText::new(tl!("Style:")).size(12.0).color(t.text).underline()).clicked() {
                        app.ui.open_panel = Some("graphicStyles".into());
                    }
                    let resp = widgets::chip_button_named(ui, 22.0, true, tl!("Graphic Style"), |ui, r| {
                        crate::panels::graphic_styles::paint_linked(app, ui, r)
                    });
                    egui::Popup::menu(&resp).show(|ui| crate::panels::graphic_styles::picker(app, ui));
                    // Recolor Artwork, when the selection has colours to recolour.
                    let colored = app.session.active().is_some_and(|st| crate::canvas::has_colors(&st.doc, &st.selection.objects));
                    if colored && widgets::icon_button(ui, "palette", tl!("Recolor Artwork"), false, 24.0).clicked() {
                        crate::menus::invoke(app, "ui.recolorDialog", json!({}));
                    }
                }
                ui.separator();
                crate::panels::character::control_bar(app, ui);
                if sel.is_empty() {
                    if widgets::flat_button(ui, tl!("Document Setup"), 112.0).clicked() {
                        app.run("file.documentSetup", json!({})).ok();
                    }
                    if widgets::flat_button(ui, tl!("Preferences"), 90.0).clicked() {
                        app.run("edit.preferences", json!({})).ok();
                    }
                    return;
                }
                crate::panels::align::align_buttons(app, ui, 24.0);
                ui.separator();
                // The Transform link opens the whole Transform panel (reference point, rotate,
                // shear, options) in a popover; X/Y/W/H follow inline while the bar has room.
                crate::panels::transform::link(app, ui);
                // The bounding box, rotated with rotated objects: its centre and its own sides.
                if let Some(b) = app.selection_box()
                    && ui.available_width() >= INLINE_TRANSFORM_WIDTH
                {
                    let c = b.center();
                    let origin = app.ruler_origin();
                    let link = crate::panels::transform::proportional(app, ui);
                    let copy = crate::panels::transform::alt_enter_copies(app, ui);
                    for (k, lbl, v, offset) in [
                        ("x", "X:", c.x, origin.x),
                        ("y", "Y:", c.y, origin.y),
                        ("width", "W:", b.rect.width(), 0.0),
                        ("height", "H:", b.rect.height(), 0.0),
                    ] {
                        // The W/H link sits between W and H.
                        if k == "height" {
                            crate::panels::transform::constrain_link(app, ui);
                        }
                        widgets::field_label(ui, egui::RichText::new(tl!(lbl)).size(12.0).color(t.text_dim));
                        let (id, shown) = (("cb", k), Some(v - offset));
                        let set = if matches!(k, "width" | "height") {
                            widgets::size_field(ui, id, shown, units, 80.0)
                        } else {
                            widgets::num_field(ui, id, shown, units, 80.0)
                        };
                        if let Some(nv) = set {
                            app.run("object.setBounds", json!({k: nv + offset, "reference": 4, "proportional": link, "copy": copy})).ok();
                        }
                    }
                }
            });
        });
}

/// A document tab's title: "Name* @ 66.67 % (RGB/Preview)", or while an opacity mask is edited
/// "Name* @ 66.67 % (<Opacity Mask>/Opacity Mask)".
fn tab_title(d: &vectorcraft_engine::DocState, zoom: f64, outline: bool, display_print_size: bool) -> String {
    let mode = if d.doc.mask_edit.is_some() {
        format!("<{0}>/{0}", tl!("Opacity Mask"))
    } else {
        let color = if d.doc.color_mode == vectorcraft_doc::ColorMode::Cmyk { "CMYK" } else { "RGB" };
        format!("{color}/{}", if outline { tl!("Outline") } else { tl!("Preview") })
    };
    format!("{}{} @ {} ({mode})", d.title(), if d.is_dirty() { "*" } else { "" }, zoom_label(zoom, display_print_size).replace('%', " %"))
}

/// Whether a document tab's × comes before its title (macOS) rather than after it (Windows,
/// Linux and the web).
const CLOSE_BEFORE_TITLE: bool = cfg!(target_os = "macos");

/// The first of tabs `widths` wide that the strip shows in `room`: the first one, or a later one so
/// that the `active` tab fits.
fn first_tab_shown(widths: &[f32], active: Option<usize>, room: f32) -> usize {
    let Some(a) = active else { return 0 };
    let mut first = 0;
    while first < a && widths.get(first..=a).map_or(0.0, |w| w.iter().sum::<f32>()) > room {
        first += 1;
    }
    first
}

/// Document tab strip: "Name* @ 66.67% (RGB/Preview)". User Interface › Large Tabs makes the tabs
/// taller, with larger titles. Tabs that don't fit are reached from a » button at the strip's
/// right end, which lists every open document, and the active tab is always in view (#746).
pub fn doc_tabs(app: &mut VectorcraftApp, ui: &mut Ui) {
    /// The width of the » button.
    const MORE: f32 = 30.0;
    let t = Tokens::get(ui.ctx());
    let (height, title_size) = if app.session.prefs.large_tabs { (44.0, 14.0) } else { (35.0, 12.5) };
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(strip, 0.0, t.tab_strip);
    ui.painter().line_segment([strip.left_bottom(), strip.right_bottom()], Stroke::new(1.0, t.border));
    // On the Home screen no tab is the current one.
    let active = app.session.active_index().filter(|_| app.ui.home.is_none());
    let tabs: Vec<(String, std::sync::Arc<egui::Galley>)> = app
        .session
        .documents()
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let zoom = app.views.get(i).map(|v| v.zoom).unwrap_or(1.0);
            let title = tab_title(d, zoom, app.ui.view.outline, app.session.prefs.display_print_size);
            let color = if Some(i) == active { t.text_strong } else { t.text_dim };
            (title.clone(), ui.painter().layout_no_wrap(title, theme::semibold(title_size), color))
        })
        .collect();
    let widths: Vec<f32> = tabs.iter().map(|(_, g)| g.size().x + 50.0).collect();
    let overflow = widths.iter().sum::<f32>() > strip.width();
    let room = strip.width() - if overflow { MORE } else { 0.0 };
    let first = if overflow { first_tab_shown(&widths, active, room) } else { 0 };
    let mut x = strip.left();
    let mut activate = None;
    let mut close = None;
    let mut hovering_art = false;
    for (i, ((_, galley), w)) in tabs.iter().zip(&widths).enumerate().skip(first) {
        if x + w > strip.left() + room && Some(i) != active {
            break;
        }
        let is_active = Some(i) == active;
        let r = egui::Rect::from_min_size(egui::pos2(x, strip.top()), vec2(*w, strip.height() - 1.0));
        let resp = ui.interact(r, ui.scope_id().with(("tab", i)), Sense::click());
        if is_active {
            ui.painter().rect_filled(r, 0.0, t.panel);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.4));
        }
        ui.painter().line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.5, t.border));
        // The × where each platform puts a tab's: before the title on macOS, after it elsewhere
        // (#673).
        let (x_center, title_left) = if CLOSE_BEFORE_TITLE { (r.left() + 16.0, r.left() + 32.0) } else { (r.right() - 16.0, r.left() + 14.0) };
        let xr = egui::Rect::from_center_size(egui::pos2(x_center, r.center().y), vec2(12.0, 12.0));
        let xresp = ui.interact(xr.expand(3.0), ui.scope_id().with(("tabx", i)), Sense::click());
        icons::paint(ui, "x", xr, if xresp.hovered() { t.text_strong } else { t.text });
        ui.painter().galley(egui::pos2(title_left, r.center().y - galley.size().y / 2.0), galley.clone(), t.text);
        hovering_art |= crate::document_drag::hover_tab(app, &resp, i, xr.expand(3.0));
        if xresp.clicked() {
            close = Some(i);
        } else if resp.clicked() {
            activate = Some(i);
        }
        x += w;
    }
    crate::document_drag::finish_tabs(ui.ctx(), hovering_art);
    // The » button: every open document, the active one checked. The list scrolls once it
    // reaches the bottom of the window, so every document of a large session stays reachable
    // (#4017).
    if overflow {
        let more = egui::Rect::from_min_max(egui::pos2(strip.right() - MORE, strip.top()), egui::pos2(strip.right(), strip.bottom() - 1.0));
        let resp = ui.interact(more, ui.scope_id().with("tabs-more"), Sense::click());
        ui.painter().rect_filled(more, 0.0, if resp.hovered() { t.hover.gamma_multiply(0.4) } else { t.tab_strip });
        icons::paint(ui, "chevrons-right", egui::Rect::from_center_size(more.center(), vec2(14.0, 14.0)), t.text);
        let resp = resp.on_hover_text(tl!("Show all open documents"));
        egui::Popup::menu(&resp).show(|ui| {
            widgets::menu_scroll(ui, |ui| {
                for (i, (title, _)) in tabs.iter().enumerate() {
                    if widgets::menu_item_name(ui, title, true, Some(i) == active) {
                        activate = Some(i);
                        ui.close();
                    }
                }
            });
        });
    }
    if let Some(i) = close {
        if let Err(e) = crate::unsaved::close(app, i) {
            app.status(e);
        }
    } else if let Some(i) = activate {
        app.ui.home = None;
        let changed = app.session.active_index() != Some(i);
        if app.session.set_active(i)
            && changed
            && app.session.tool_id() == "artboard"
            && let Some(n) = app.session.active().map(|d| d.doc.artboards.len())
        {
            // A document switch recreates the tool; restore the displayed board as its target.
            // Clicking the current tab keeps its existing multiselection and tool options.
            let selected = crate::panels::artboards::selected(app, n);
            crate::panels::artboards::select(app, selected);
        }
    }
}

/// The isolation mode breadcrumb bar. It is drawn over the top of `area` (the canvas), after the
/// canvas, so entering isolation mode does not change the canvas's size (#3126).
pub fn isolation_bar(app: &mut VectorcraftApp, ui: &mut Ui, area: egui::Rect) {
    let t = Tokens::get(ui.ctx());
    if let Some(st) = app.session.active()
        && let Some(iso) = st.isolation
    {
        let mut crumbs: Vec<String> =
            st.doc.ancestry(iso).unwrap_or_default().iter().filter_map(|id| st.doc.node(*id)).map(|n| n.display_name()).collect();
        // Pattern editing mode: the pattern's name, and Save a Copy / Done / Cancel at the right.
        let pattern_edit = st.doc.pattern_edit.as_ref().map(|e| e.pattern.clone());
        if let Some(pn) = &pattern_edit {
            crumbs.push(pn.clone());
        }
        let bar = egui::Rect::from_min_size(area.min, vec2(area.width(), 24.0));
        ui.interact(bar, ui.scope_id().with("iso-bar"), Sense::click_and_drag());
        ui.painter().rect_filled(bar, 0.0, t.panel);
        let back = egui::Rect::from_min_size(bar.min + vec2(6.0, 3.0), vec2(18.0, 18.0));
        let bresp = ui.interact(back, ui.scope_id().with("iso-back"), Sense::click());
        icons::paint(ui, "chevron-left", back, if bresp.hovered() { t.text } else { t.icon });
        ui.painter().text(
            bar.left_center() + vec2(30.0, 0.0),
            egui::Align2::LEFT_CENTER,
            crumbs.join("  ›  "),
            egui::FontId::proportional(12.0),
            t.text,
        );
        let mut pattern_cmd = None;
        if pattern_edit.is_some() {
            let mut x = bar.right() - 6.0;
            for (label, cmd) in [("Cancel", "object.pattern.cancel"), ("Done", "object.pattern.done"), ("Save a Copy", "object.pattern.saveCopy")] {
                let label = tl!(label);
                let w = 12.0 + 7.0 * label.len() as f32;
                let r = egui::Rect::from_min_max(egui::pos2(x - w, bar.top() + 3.0), egui::pos2(x, bar.bottom() - 3.0));
                let resp = ui.interact(r, ui.scope_id().with(("pat-bar", cmd)), Sense::click());
                ui.painter().rect_filled(r, 3.0, if resp.hovered() { t.hover } else { t.panel });
                ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, t.button_border), egui::StrokeKind::Inside);
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), t.text);
                if resp.clicked() {
                    pattern_cmd = Some(cmd);
                }
                x -= w + 6.0;
            }
        }
        if bresp.clicked() {
            pattern_cmd = pattern_cmd.or(pattern_edit.as_ref().map(|_| "object.pattern.done"));
            if pattern_cmd.is_none() {
                app.run("object.exitIsolation", json!({})).ok();
            }
        }
        if let Some(cmd) = pattern_cmd {
            app.run(cmd, json!({})).ok();
        }
    }
}

/// Draws the artboard navigator's field, which shows the active artboard (`cur`) by number and
/// name. The field's list shows every artboard by number and name, as the Artboards panel lists
/// them, and opens with the active artboard's row in view. The list lays out only the rows in view
/// and at most one more. Returns the artboard chosen in the list.
fn artboard_list(app: &VectorcraftApp, ui: &mut Ui, cur: usize) -> Option<usize> {
    /// The width of the field.
    const FIELD_W: f32 = 140.0;
    /// The least width of the list.
    const LIST_W: f32 = 220.0;
    /// The height of a row, as in the Artboards panel.
    const ROW_H: f32 = 24.0;
    /// The rows the list shows at once.
    const ROWS: f32 = 10.0;
    let t = Tokens::get(ui.ctx());
    let boards = app.session.active().map(|d| d.doc.artboards.as_slice()).unwrap_or_default();
    let shown = boards.get(cur).map_or_else(|| "–".to_string(), |a| format!("{}  {}", cur + 1, a.name));
    // Under `drawn`, egui's temp data stores the pass the list was last drawn in.
    let drawn = ui.scope_id().with("artboard-list");
    let height = ROW_H * ROWS;
    ui.add_enabled_ui(!boards.is_empty(), |ui| {
        ui.set_max_width(FIELD_W);
        ui.name_inputs(tl!("Artboard"), |ui| {
            egui::ComboBox::from_id_salt("artboard-list")
                .selected_text(egui::RichText::new(shown).size(11.5))
                .width(FIELD_W)
                .wrap_mode(egui::TextWrapMode::Truncate)
                .height(height)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    let pass = ui.ctx().cumulative_pass_nr();
                    let last = ui.data(|d| d.get_temp::<u64>(drawn)).unwrap_or_default();
                    ui.data_mut(|d| d.insert_temp(drawn, pass));
                    let mut area = egui::ScrollArea::vertical().max_height(height);
                    // As the list opens, it scrolls the active row as close to the middle as the list allows.
                    if last + 1 < pass || last == 0 {
                        area = area.vertical_scroll_offset((cur as f32 * ROW_H - (height - ROW_H) * 0.5).max(0.0));
                    }
                    // Every number ends where the widest one ends.
                    let numbers = ui.painter().layout_no_wrap(boards.len().to_string(), egui::FontId::proportional(12.0), t.text_dim).size().x;
                    let width = ui.available_width().max(LIST_W);
                    let mut chosen = None;
                    ui.spacing_mut().item_spacing.y = 0.0;
                    area.show_rows(ui, ROW_H, boards.len(), |ui, range| {
                        for i in range {
                            let Some(board) = boards.get(i) else { continue };
                            let (r, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                            if i == cur {
                                ui.painter().rect_filled(r, 0.0, t.row_selected);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(r, 0.0, t.hover);
                            }
                            let number_end = r.left() + 8.0 + numbers;
                            ui.painter().text(
                                egui::pos2(number_end, r.center().y),
                                egui::Align2::RIGHT_CENTER,
                                (i + 1).to_string(),
                                egui::FontId::proportional(12.0),
                                t.text_dim,
                            );
                            let name_start = number_end + 10.0;
                            let name = egui::WidgetText::from(egui::RichText::new(&board.name).size(12.5)).into_galley(
                                ui,
                                Some(egui::TextWrapMode::Truncate),
                                r.right() - 8.0 - name_start,
                                egui::TextStyle::Body,
                            );
                            ui.painter().galley(egui::pos2(name_start, r.center().y - name.size().y / 2.0), name, t.text);
                            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, i == cur, format!("{}  {}", i + 1, board.name)));
                            if resp.clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                    if chosen.is_some() {
                        ui.close();
                    }
                    chosen
                })
                .inner
                .flatten()
        })
    })
    .inner
}

/// Draw the status message, bounded readouts and artboard navigation controls.
pub fn status_bar(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("status_bar")
        .exact_size(24.0)
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(8, 0)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let zoom = app.view().map(|v| v.zoom).unwrap_or(1.0);
                let print = app.session.prefs.display_print_size;
                let shown = crate::state::zoom_display_percent(zoom, print);
                ui.name_inputs(tl!("Zoom"), |ui| {
                    egui::ComboBox::from_id_salt("zoom-combo")
                        .selected_text(egui::RichText::new(zoom_label(zoom, print)).size(11.5))
                        .width(76.0)
                        .show_ui(ui, |ui| {
                            for z in ZOOM_STOPS.iter().rev() {
                                if ui
                                    .selectable_label((z - shown).abs() < 0.01, zoom_label(crate::state::zoom_from_display_percent(*z, print), print))
                                    .clicked()
                                {
                                    app.run("view.setZoom", json!({"zoom": z})).ok();
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, tl!("Fit on Screen")).clicked() {
                                app.run("view.fitArtboard", json!({})).ok();
                            }
                            if ui.selectable_label(false, tl!("Fit All")).clicked() {
                                app.run("view.fitAll", json!({})).ok();
                            }
                        });
                });
                let rot = app.view().map(|v| v.rotation).unwrap_or(0.0);
                ui.name_inputs(tl!("Rotation"), |ui| {
                    egui::ComboBox::from_id_salt("rot-combo")
                        .selected_text(egui::RichText::new(format!("{rot:.0}°")).size(11.5))
                        .width(52.0)
                        .show_ui(ui, |ui| {
                            for (label, a) in ROTATE_VIEW_ANGLES {
                                if ui.selectable_label((rot - a).abs() < 1e-9, label).clicked() {
                                    // It fails only without a document, when there is no view to turn.
                                    app.run("view.rotate", json!({"angle": a})).ok();
                                }
                            }
                        });
                });
                ui.separator();
                // The artboard navigator has First and Previous, a field with the current artboard's
                // number and name and a list of every artboard, then Next and Last.
                let nab = app.session.active().map(|d| d.doc.artboards.len()).unwrap_or(0);
                let cur = crate::panels::artboards::selected(app, nab);
                let mut go = None;
                for (icon, to, tip) in [("chevrons-left", "first", tl!("First Artboard")), ("chevron-left", "previous", tl!("Previous Artboard"))] {
                    if widgets::icon_button_enabled(ui, icon, tip, false, cur > 0, 18.0).clicked() {
                        go = Some(json!(to));
                    }
                }
                if let Some(i) = artboard_list(app, ui, cur) {
                    go = Some(json!(i));
                }
                for (icon, to, tip) in [("chevron-right", "next", tl!("Next Artboard")), ("chevrons-right", "last", tl!("Last Artboard"))] {
                    if widgets::icon_button_enabled(ui, icon, tip, false, cur + 1 < nab, 18.0).clicked() {
                        go = Some(json!(to));
                    }
                }
                if let Some(to) = go
                    && let Err(e) = app.run("view.goToArtboard", json!({ "index": to }))
                {
                    app.status(e);
                }
                ui.separator();
                let tool = vectorcraft_tools::tool_info(app.session.tool_id())
                    .map(|t| {
                        // The catalog keys carry the full label ("Selection Tool"); only English drops the suffix.
                        let shown = tl!(t.label);
                        if crate::i18n::current() == crate::i18n::Lang::EN { shown.trim_end_matches(" Tool") } else { shown }
                    })
                    .unwrap_or("");
                ui.label(egui::RichText::new(tool).size(11.5).color(t.text_dim));
                status_readouts(app, ui, &t);
            });
        });
}

/// The most recently measured status widths, scoped to this UI and display settings.
struct StatusReadoutWidths {
    key: (crate::i18n::Lang, vectorcraft_doc::Unit, f32),
    values: [f32; 4],
}

/// Reuse sample-string measurements until language, document units or display scale changes.
/// Keep a single entry per UI so repeated DPI/zoom changes cannot grow the cache indefinitely.
fn status_readout_widths(ui: &Ui, lang: crate::i18n::Lang, unit: vectorcraft_doc::Unit, t: &Tokens) -> Arc<StatusReadoutWidths> {
    let id = ui.scope_id().with("status-readout-widths");
    let key = (lang, unit, ui.ctx().pixels_per_point());
    if let Some(cached) = ui.data(|data| data.get_temp::<Arc<StatusReadoutWidths>>(id))
        && cached.key == key
    {
        return cached;
    }
    // Text layout also accesses egui state, so measure outside the ui.data lock.
    let mono = theme::mono(10.5);
    let proportional = egui::FontId::proportional(10.5);
    let render = crate::i18n::tr_ctx(lang, "diagnostics", "render");
    let ui_name = crate::i18n::tr_ctx(lang, "diagnostics", "ui");
    let measure = |text, font: &egui::FontId| ui.painter().layout_no_wrap(text, font.clone(), t.text).size().x.ceil() + 4.0;
    // Reserve a readable numeric range using every digit in the active font. These samples
    // depend on the language/units, never the live values: timings cannot move field boundaries.
    let timing_width = |label| (0..=9).map(|d| measure(format!("{label} {d}{d}{d}{d}.{d} ms"), &proportional)).fold(0.0_f32, f32::max);
    let render_width = timing_width(render);
    let ui_width = timing_width(ui_name);
    let fps_width = (0..=9).map(|d| measure(format!("{d}{d}{d}{d} fps"), &proportional)).fold(0.0_f32, f32::max);
    // Each axis shows at least -9999.99..9999.99 in the selected unit without elision.
    let axis_width = (0..=9).map(|d| measure(format!("X: -{d}{d}{d}{d}.{d}{d} {}", unit.suffix()), &mono)).fold(100.0_f32, f32::max);
    let widths = Arc::new(StatusReadoutWidths { key, values: [render_width, ui_width, fps_width, axis_width] });
    ui.data_mut(|data| data.insert_temp(id, Arc::clone(&widths)));
    widths
}

/// Allocate the remaining row once, then paint each field inside its own bounded rectangle.
/// Child UIs requested with a desired width can shrink to their contents; these rectangles cannot.
fn status_readouts(app: &VectorcraftApp, ui: &mut Ui, t: &Tokens) {
    const MESSAGE_MIN: f32 = 180.0;
    let mono = theme::mono(10.5);
    let proportional = egui::FontId::proportional(10.5);
    let unit = app.session.general_unit();
    let lang = crate::i18n::current();
    let render = crate::i18n::tr_ctx(lang, "diagnostics", "render");
    let ui_name = crate::i18n::tr_ctx(lang, "diagnostics", "ui");
    let widths = status_readout_widths(ui, lang, unit, t);
    let [render_width, ui_width, fps_width, axis_width] = widths.values;
    let coord_width = 2.0 * axis_width;
    let perf_width = render_width + ui_width + fps_width + 12.0;
    let point_id = ui.scope_id().with(("status-hover-point", app.session.active().map(|st| st.uid)));
    if let Some(p) = app.hover_doc {
        ui.data_mut(|data| data.insert_temp(point_id, p));
    }
    let gap = ui.spacing().item_spacing.x;
    let size = vec2(ui.available_width().max(0.0), ui.available_height().max(0.0));
    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
    let coordinates = area.width() >= MESSAGE_MIN + coord_width + gap;
    let diagnostics = area.width() >= MESSAGE_MIN + coord_width + perf_width + 2.0 * gap;
    let mut right = area.right();
    if diagnostics {
        let perf = egui::Rect::from_min_max(egui::pos2(right - perf_width, area.top()), area.right_bottom());
        let mut x = perf.left();
        for (id, width, text) in [
            ("render", render_width, format!("{render} {:.1} ms", app.perf.render_ms)),
            ("ui", ui_width, format!("{ui_name} {:.1} ms", app.perf.frame_ms)),
            // No rate until the window has drawn continuously for a moment (`frame_rate`).
            ("fps", fps_width, if app.perf.fps > 0.0 { format!("{:.0} fps", app.perf.fps) } else { "– fps".into() }),
        ] {
            let rect = egui::Rect::from_min_size(egui::pos2(x, area.top()), vec2(width, area.height()));
            status_field(ui, id, rect, text, proportional.clone(), t.text_disabled);
            x += width + 6.0;
        }
        right = perf.left() - gap;
    }
    if coordinates {
        let left = right - coord_width;
        // Keep the coordinate slot even without a hover point, so entering/leaving the canvas
        // cannot move or re-truncate the message. X and Y have separate, stable origins.
        let coord_rect = egui::Rect::from_min_max(egui::pos2(left, area.top()), egui::pos2(right, area.bottom()));
        // The canvas clears hover_doc when the pointer leaves. While inspecting a readout,
        // retain its last value so its tooltip can show the complete coordinate.
        let inspecting = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| coord_rect.contains(p)) && ui.rect_contains_pointer(coord_rect);
        let point = app.hover_doc.or_else(|| inspecting.then(|| ui.data(|data| data.get_temp::<vectorcraft_geom::Point>(point_id))).flatten());
        if let Some(p) = point {
            let origin = app.ruler_origin();
            for (id, x, value) in [("X", left, p.x - origin.x), ("Y", left + axis_width, p.y - origin.y)] {
                let rect = egui::Rect::from_min_size(egui::pos2(x, area.top()), vec2(axis_width, area.height()));
                status_field(ui, id, rect, format!("{id}: {}", unit.readout(value)), mono.clone(), t.text_dim);
            }
        }
        right = left - gap;
    }
    let busy: Vec<&str> = app
        .background
        .jobs
        .iter()
        .filter(|j| !j.label.is_empty() && !j.cancelled())
        .map(|j| j.label.as_str())
        .chain(app.opening.pending.iter().map(|p| p.label.as_str()))
        .chain(app.placing.pending.iter().map(|p| p.label.as_str()))
        .collect();
    // Reserve the spinner's space while idle too: starting a background job changes the text,
    // not its origin. Messages take priority over optional readouts in a narrow window.
    let message = if let Some(job) = busy.first() {
        let spinner = egui::Rect::from_center_size(egui::pos2(area.left() + 6.0, area.center().y), vec2(12.0, 12.0));
        ui.put(spinner, egui::Spinner::new().size(12.0).color(t.accent));
        let label = crate::i18n::msg(job);
        let more = busy.len() - 1;
        if more > 0 { format!("{label}… (+{more})") } else { format!("{label}…") }
    } else {
        crate::i18n::msg(&app.ui.status)
    };
    if !message.is_empty() {
        let rect = egui::Rect::from_min_max(egui::pos2((area.left() + 18.0).min(right), area.top()), egui::pos2(right, area.bottom()));
        status_field(ui, "message", rect, message, egui::FontId::proportional(11.0), t.text);
    }
}

/// A field never grows with its text. Clipping also bounds over-wide glyphs; hover and access
/// information retain the complete value when its on-screen label is truncated.
fn status_field(ui: &mut Ui, id: &str, rect: egui::Rect, text: String, font: egui::FontId, color: egui::Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let galley = egui::WidgetText::from(egui::RichText::new(&text).font(font)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    let at = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let elided = galley.elided;
    ui.painter().with_clip_rect(ui.clip_rect().intersect(rect)).galley(at, galley, color);
    let response = ui.interact(rect, ui.scope_id().with(("status-field", id)), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &text));
    if elided {
        response.on_hover_text(text);
    }
}

/// Contextual hint for the active tool: segments of (text, bold). Keys in bold segments are
/// written as shortcuts are ("Alt+Click"); [`hint_segments`] names them for the platform.
/// The hint of the Zoom tool with Performance › Animated Zoom on.
const ANIMATED_ZOOM_HINT: &str = "zoom.animated";

fn hint_for(tool: &str) -> Option<&'static [(&'static str, bool)]> {
    Some(match tool {
        "dimension" => &[
            ("Drag", true),
            (" out from a highlighted edge to place its dimension  |  ", false),
            ("Esc", true),
            (" cancels  |  Double-click the tool for options", false),
        ],
        "selection" => &[
            ("Click", true),
            (" the object to select  |  ", false),
            ("Shift+Click", true),
            (" to select multiple objects  |  ", false),
            ("Alt+Drag", true),
            (" the object to duplicate", false),
        ],
        "directSelection" => &[
            ("Click", true),
            (" an anchor or path segment to select it  |  ", false),
            ("Drag", true),
            (" to move  |  ", false),
            ("Shift+Click", true),
            (" to add", false),
        ],
        "pen" => &[
            ("Click", true),
            (" to add a corner point  |  ", false),
            ("Drag", true),
            (" to add a smooth point  |  ", false),
            ("Click the first point", true),
            (" to close  |  ", false),
            ("Enter", true),
            (" to finish", false),
        ],
        "curvature" => &[
            ("Click", true),
            (" to add smooth points  |  ", false),
            ("Double-click", true),
            (" to toggle corner  |  ", false),
            ("Esc", true),
            (" to finish", false),
        ],
        "type" => &[
            ("Click", true),
            (" to create point type  |  ", false),
            ("Drag", true),
            (" to create area type  |  ", false),
            ("Esc", true),
            (" to exit editing", false),
        ],
        "rectangle" | "roundedRectangle" | "ellipse" | "polygon" | "star" => &[
            ("Drag", true),
            (" to draw  |  ", false),
            ("Shift+Drag", true),
            (" to constrain proportions  |  ", false),
            ("Alt+Drag", true),
            (" from center  |  ", false),
            ("Click", true),
            (" for exact size", false),
        ],
        "rotate" | "reflect" | "scale" | "shear" => &[
            ("Click", true),
            (" to set the reference point  |  ", false),
            ("Drag", true),
            (" to transform  |  ", false),
            ("Alt+Click", true),
            (" for exact values", false),
        ],
        "hand" => &[("Drag", true), (" to pan the view", false)],
        "zoom" => &[
            ("Click", true),
            (" to zoom in  |  ", false),
            ("Alt+Click", true),
            (" to zoom out  |  ", false),
            ("Drag", true),
            (" to zoom into an area", false),
        ],
        ANIMATED_ZOOM_HINT => &[
            ("Click", true),
            (" to zoom in  |  ", false),
            ("Alt+Click", true),
            (" to zoom out  |  ", false),
            ("Drag", true),
            (" right or left to zoom in or out  |  ", false),
            ("Hold", true),
            (" to keep zooming", false),
        ],
        "eyedropper" => &[
            ("Click", true),
            (" an object to copy its attributes  |  ", false),
            ("Alt+Click", true),
            (" to apply the selection's to it  |  ", false),
            ("Shift+Click", true),
            (" to sample a color", false),
        ],
        "gradient" => &[("Drag", true), (" across a selected object to set the gradient direction", false)],
        "artboard" => &[
            ("Click", true),
            (" to select an artboard  |  ", false),
            ("Drag", true),
            (" on the canvas to create one (", false),
            ("Shift", true),
            (" selects several, or draws one inside an artboard)", false),
        ],
        "width" => &[
            ("Drag", true),
            (" a stroke to add a width point  |  ", false),
            ("Alt+Drag", true),
            (" to change one side  |  ", false),
            ("Shift+Click", true),
            (" to select more points  |  ", false),
            ("Double-click", true),
            (" a point to edit it  |  ", false),
            ("Delete", true),
            (" to remove points", false),
        ],
        "puppetWarp" => &[
            ("Click", true),
            (" the art to add a pin  |  ", false),
            ("Shift+Click", true),
            (" to select more pins  |  ", false),
            ("Drag", true),
            (" a pin to warp  |  ", false),
            ("Alt+Drag", true),
            (" near a selected pin to rotate  |  ", false),
            ("Delete", true),
            (" to remove pins", false),
        ],
        "warp" => &[
            ("Drag", true),
            (" across paths to push them along  |  ", false),
            ("Alt+Drag", true),
            (" to size the brush (", false),
            ("Shift", true),
            (" keeps its proportions)", false),
        ],
        "twirl" | "pucker" | "bloat" => &[
            ("Click or drag", true),
            (" over paths, and hold still to keep going  |  ", false),
            ("Alt+Drag", true),
            (" to size the brush (", false),
            ("Shift", true),
            (" keeps its proportions)", false),
        ],
        "scallop" | "crystallize" | "wrinkle" => &[
            ("Click or drag", true),
            (" over paths to roughen their outlines  |  ", false),
            ("Alt+Drag", true),
            (" to size the brush (", false),
            ("Shift", true),
            (" keeps its proportions)", false),
        ],
        "perspectiveSelection" => &[
            ("Drag", true),
            (" to move in perspective  |  ", false),
            ("Alt+Drag", true),
            (" to copy  |  ", false),
            ("5", true),
            (" while dragging to move perpendicular to the plane  |  ", false),
            ("Drag a handle", true),
            (" to scale in perspective", false),
        ],
        "blend" => &[
            ("Click", true),
            (" an object, then another to blend them  |  ", false),
            ("Click an anchor point", true),
            (" to blend from it  |  ", false),
            ("Alt+Click", true),
            (" to set spacing and orientation", false),
        ],
        _ => return None,
    })
}

/// The hint bar's segments for `tool`, with keys named as the menus name them (Alt and Ctrl on
/// Windows and Linux, ⌥ and ⌘ on macOS). Tools without a hint point to Search Commands.
/// Every hint-bar fragment (the action after the last `+` of a chord included), so the catalog tests
/// can insist a complete language covers them.
pub fn hint_strings() -> Vec<&'static str> {
    let mut v = Vec::new();
    for tool in vectorcraft_tools::catalog::all_tools().map(|t| t.id).chain([ANIMATED_ZOOM_HINT]) {
        for &(text, bold) in hint_for(tool).unwrap_or(&[]) {
            let text = if bold { text.rsplit_once('+').map_or(text, |(_, k)| k) } else { text };
            if !text.is_empty() && !text.chars().all(|c| c.is_ascii_digit()) && !v.contains(&text) {
                v.push(text);
            }
        }
    }
    v
}

fn hint_segments(tool: &str) -> Vec<(Cow<'static, str>, bool)> {
    let search;
    let segments = match hint_for(tool) {
        Some(s) => s,
        None => {
            search = match menus::shortcut_of("help.commandPalette") {
                Some(sc) => [(tl!("Press "), false), (sc, true), (tl!(" to search every command"), false)],
                None => [(tl!("Use "), false), (tl!("Help › Search Commands…"), true), (tl!(" to search every command"), false)],
            };
            &search
        }
    };
    segments
        .iter()
        .map(|&(text, bold)| {
            // Key names ("Click", "Esc") translate; in a chord ("Alt+Drag") the modifiers are named for
            // the platform and only the action after the last `+` translates.
            if !bold {
                return (Cow::Borrowed(tl!(text)), bold);
            }
            match text.rsplit_once('+') {
                Some((_, key)) if !key.is_empty() => {
                    let pretty = menus::pretty_shortcut(text);
                    let shown = pretty.strip_suffix(key).map_or(pretty.clone(), |m| format!("{m}{}", tl!(key)));
                    (Cow::Owned(shown), bold)
                }
                _ => (Cow::Owned(menus::pretty_shortcut(tl!(text))), bold),
            }
        })
        .collect()
}

pub fn hint_bar(app: &mut VectorcraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let panel = egui::Panel::bottom("hint_bar")
        .exact_size(28.0)
        .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(10, 0)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                ui.painter().circle_stroke(r.center(), 7.0, Stroke::new(1.2, t.text));
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "?", theme::semibold(11.0), t.text);
                ui.add_space(6.0);
                let mut job = egui::text::LayoutJob::default();
                let segments = if let Some(d) = app.ui.dialog.as_ref().filter(|d| crate::dialogs::revolve_gizmo::active(d)) {
                    vec![(crate::dialogs::revolve_gizmo::hint(d).into(), false)]
                } else {
                    let tool = app.session.tool_id();
                    let hint = if tool == "zoom" && crate::canvas::animated_zoom(&app.session.prefs) { ANIMATED_ZOOM_HINT } else { tool };
                    hint_segments(hint)
                };
                for (txt, bold) in segments {
                    let font = if bold { theme::semibold(12.5) } else { egui::FontId::proportional(12.5) };
                    job.append(&txt, 0.0, egui::TextFormat { font_id: font, color: if bold { t.text_strong } else { t.text }, ..Default::default() });
                }
                ui.label(job);
            });
        });
    widgets::set_menu_floor(ui.ctx(), panel.response.rect.top());
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod visual_regressions {
    use super::*;

    fn scaled_input(size: egui::Vec2, scale: f32) -> egui::RawInput {
        let mut input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(scale);
        input
    }

    struct NativeMenus;
    impl crate::native_menu::Backend for NativeMenus {
        fn sync(&mut self, _: &crate::native_menu::MenuBar) {}
        fn drain(&mut self) -> Vec<crate::native_menu::Event> {
            Vec::new()
        }
    }

    #[test]
    fn capture_chrome_visual_fixtures() {
        let Some(directory) = std::env::var_os("CRAFT_UI_VISUAL_FIXTURES").map(std::path::PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&directory).unwrap();
        for brightness in theme::Brightness::ALL {
            for width in [800.0, 1280.0] {
                for scale in [1.0, 1.5, 2.0] {
                    let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Default::default());
                    app.services.native_menu = Some(crate::native_menu::NativeMenu::new(Box::new(NativeMenus)));
                    app.ui.status = format!("Saved /Users/example/Documents/{}/illustration.svg", "long-folder-name/".repeat(10));
                    let mut ready = false;
                    let mut harness =
                        egui_kittest::Harness::builder().with_size(vec2(width, 200.0)).with_pixels_per_point(scale).wgpu().build_ui_state(
                            move |ui, app: &mut VectorcraftApp| {
                                if !ready {
                                    egui_extras::install_image_loaders(ui.ctx());
                                    theme::install_fonts(ui.ctx());
                                    theme::apply(ui.ctx(), brightness);
                                    ready = true;
                                    return;
                                }
                                app_bar(app, ui);
                                status_bar(app, ui);
                            },
                            app,
                        );
                    harness.input_mut().max_texture_side = Some(8192);
                    harness.run_steps(4);
                    let stem = format!("vectorcraft-chrome-{brightness:?}-{width}-{scale}x");
                    harness.render().unwrap().save(directory.join(format!("{stem}.png"))).unwrap();
                    let mut regions = serde_json::Map::new();
                    for id in ["app_bar", "status_bar"] {
                        let rect = egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::unique(id)).unwrap().outer_rect;
                        regions.insert(id.into(), json!([rect.left(), rect.top(), rect.width(), rect.height()]));
                    }
                    let dynamic: Vec<_> = harness
                        .output()
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.job.text.contains(" fps") => {
                                let r = text.visual_bounding_rect();
                                Some(json!([r.left(), r.top(), r.width(), r.height()]))
                            }
                            _ => None,
                        })
                        .collect();
                    std::fs::write(
                        directory.join(format!("{stem}.json")),
                        serde_json::to_vec_pretty(&json!({
                            "screen": [width, 200.0], "scale": scale, "regions": regions, "native_dynamic_text_regions": dynamic
                        }))
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn native_title_does_not_overlap_header_controls() {
        for brightness in theme::Brightness::ALL {
            for width in [800.0, 1280.0] {
                for scale in [1.0, 1.5, 2.0] {
                    let ctx = egui::Context::default();
                    egui_extras::install_image_loaders(&ctx);
                    theme::install_fonts(&ctx);
                    theme::apply(&ctx, brightness);
                    let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Default::default());
                    app.services.native_menu = Some(crate::native_menu::NativeMenu::new(Box::new(NativeMenus)));
                    let mut output = ctx.run_ui(scaled_input(vec2(width, 640.0), scale), |ui| app_bar(&mut app, ui));
                    output.textures_delta.clear();
                    assert_eq!(ctx.content_rect().size(), vec2(width, 640.0));
                    assert_eq!(ctx.pixels_per_point(), scale);
                    let texts: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    let title = texts.iter().find(|text| text.galley.job.text == "VectorCraft");
                    if width >= 1280.0 {
                        assert!(title.is_some(), "the title remains visible when the header has room");
                    }
                    if let Some(title) = title {
                        for text in texts.iter().filter(|text| text.galley.job.text != "VectorCraft") {
                            assert!(
                                !title.visual_bounding_rect().intersects(text.visual_bounding_rect()),
                                "{width}: title overlaps {:?}",
                                text.galley.job.text
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn long_status_stays_between_navigation_and_performance_readouts() {
        for brightness in theme::Brightness::ALL {
            for width in [800.0, 1280.0] {
                for scale in [1.0, 1.5, 2.0] {
                    let ctx = egui::Context::default();
                    egui_extras::install_image_loaders(&ctx);
                    theme::install_fonts(&ctx);
                    theme::apply(&ctx, brightness);
                    let mut app = VectorcraftApp::new(vectorcraft_engine::Session::new(), Default::default());
                    app.ui.status = format!("Saved /Users/example/Documents/{}/illustration.svg", "long-folder-name/".repeat(10));
                    let mut output = ctx.run_ui(scaled_input(vec2(width, 640.0), scale), |ui| status_bar(&mut app, ui));
                    output.textures_delta.clear();
                    assert_eq!(ctx.content_rect().size(), vec2(width, 640.0));
                    assert_eq!(ctx.pixels_per_point(), scale);
                    let texts: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    let status = texts.iter().find(|text| text.galley.job.text.starts_with("Saved ")).expect("status stays visible");
                    assert_eq!(status.galley.rows.len(), 1, "status is one line");
                    assert!(status.galley.elided, "long status retains the full text for the label tooltip");
                    let bounds = status.visual_bounding_rect();
                    assert!(bounds.left() >= 0.0 && bounds.right() <= width, "{width}: {bounds:?}");
                    for text in texts.iter().filter(|text| !text.galley.job.text.starts_with("Saved ")) {
                        assert!(!bounds.intersects(text.visual_bounding_rect()), "{width}: status overlaps {:?}", text.galley.job.text);
                    }
                }
            }
        }
    }
}
