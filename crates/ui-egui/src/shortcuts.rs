//! Keyboard shortcuts: command shortcuts from the registry, single-key tool shortcuts, arrows,
//! and tool keys (Enter/Esc/↑/↓ while drawing).

use craft_keymap::egui::Hit;
use craft_keymap::{Active, CommandRef};
use egui::{Key, KeyboardShortcut, Modifiers};
use serde_json::json;
use vectorcraft_engine::cmd::clipboard::{Flavour, TEXT};
use vectorcraft_engine::{Result as EngineResult, UiRequest, ViewInfo};
use vectorcraft_tools::{Mods, SurroundingUnit, ToolKey};

use crate::VectorcraftApp;

/// Parse "Cmd+Shift+]" into an egui shortcut. `Cmd` is Command on macOS and Ctrl elsewhere.
/// `+` and `=` name one chord key ([`Key::Equals`]): `+` is Shift+`=` on many layouts, so a
/// `Cmd+=` chord also answers `+` however it is typed.
pub fn parse(s: &str) -> Option<KeyboardShortcut> {
    if s.is_empty() {
        return None;
    }
    // A bare `+` (the Add Anchor Point tool's key) is the key itself, not a separator with
    // nothing on either side.
    let (mods_part, key_part) = if s == "+" {
        ("", "+")
    } else if let Some(stripped) = s.strip_suffix("++") {
        (stripped.trim_end_matches('+'), "+")
    } else {
        s.rsplit_once('+').unwrap_or(("", s))
    };
    let mut m = Modifiers::NONE;
    for part in mods_part.split('+').filter(|p| !p.is_empty()) {
        match part {
            "Cmd" => m |= Modifiers::COMMAND,
            "Shift" => m |= Modifiers::SHIFT,
            "Alt" => m |= Modifiers::ALT,
            "Ctrl" => m |= Modifiers::CTRL,
            _ => return None,
        }
    }
    let key = match key_part {
        "]" => Key::CloseBracket,
        "[" => Key::OpenBracket,
        ";" => Key::Semicolon,
        "'" => Key::Quote,
        "/" => Key::Slash,
        "\\" => Key::Backslash,
        "=" | "+" => Key::Equals,
        "-" => Key::Minus,
        "Delete" => Key::Delete,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "~" => Key::Backtick,
        // A modifier alone is never a chord's key: it couldn't fire (#487).
        k => Key::from_name(k).filter(|k| !is_modifier(*k))?,
    };
    Some(KeyboardShortcut::new(m, key))
}

/// The modifier keys, which egui also reports as key presses of their own.
pub(crate) fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight | Key::AltLeft | Key::AltRight | Key::SuperLeft | Key::SuperRight
    )
}

/// Every command shortcut in effect (user overrides from Edit → Keyboard Shortcuts win), with the
/// command's params: `{}`, or `{panel}` for the panel shortcuts (`window.panel`).
pub(crate) fn all_shortcuts() -> Vec<(KeyboardShortcut, &'static str, serde_json::Value)> {
    let mut v = vec![];
    for c in vectorcraft_engine::command_specs() {
        if let Some(sc) = crate::menus::shortcut_of(c.id).and_then(parse) {
            v.push((sc, c.id, json!({})));
        }
    }
    for c in crate::menus::UI_COMMANDS {
        if let Some(sc) = crate::menus::shortcut_of(c.0).and_then(parse) {
            v.push((sc, c.0, json!({})));
        }
    }
    for (panel, _) in crate::state::all_panels() {
        if let Some(sc) = crate::shortcut_editor::panel_shortcut(panel).and_then(parse) {
            v.push((sc, "window.panel", json!({ "panel": panel })));
        }
    }
    // Most specific (most modifiers) first: `paste_command` takes the first V chord the held keys
    // match logically, so Cmd+Shift+V isn't taken by Cmd+V.
    v.sort_by_key(|(sc, ..)| {
        std::cmp::Reverse(sc.modifiers.shift as u8 + sc.modifiers.alt as u8 + sc.modifiers.command as u8 + sc.modifiers.ctrl as u8)
    });
    v
}

/// Keys that paste with Cmd, or alone. (Shift+Insert is left out: Ctrl+Insert copies.)
const PASTE_KEYS: [Key; 2] = [Key::V, Key::Paste];

/// The paste command whose V chord `held` holds (Paste in Place for Cmd+Shift+V by default), else
/// Paste: egui reports every Cmd+V chord as a paste, never as its key.
pub(crate) fn paste_command(held: Modifiers) -> &'static str {
    all_shortcuts()
        .into_iter()
        .find(|(sc, id, _)| sc.logical_key == Key::V && id.starts_with("edit.paste") && held.matches_logically(sc.modifiers))
        .map_or("edit.paste", |(_, id, _)| id)
}

/// Does every press of `key` this frame hold a chord with modifiers that a command shortcut is bound
/// to, matched as [`consume`] matches it? `cmd_only`: only chords with Cmd count. False when `key`
/// isn't pressed.
fn runs_a_command(i: &egui::InputState, key: Key, cmd_only: bool) -> bool {
    let presses: Vec<Modifiers> = i
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key => Some(*modifiers),
            _ => None,
        })
        .collect();
    let shortcuts = all_shortcuts();
    let bound = |m: &Modifiers| {
        shortcuts
            .iter()
            .any(|(sc, ..)| sc.logical_key == key && (sc.modifiers.command || (!cmd_only && sc.modifiers.any())) && m.matches_logically(sc.modifiers))
    };
    !presses.is_empty() && presses.iter().all(bound)
}

/// Whether a key press runs a shortcut of the keymap in effect, the user's own included. The web
/// build asks this of every Cmd and Ctrl press, before egui sees it, to cancel what the browser
/// would also do with it (zoom the page on `Cmd+=`, undo on `Cmd+Z`, go back on `Cmd+[`).
/// `key` and `code` are the browser's names for the character typed and the key's position, which
/// egui reads with [`Key::from_name`]; `modifiers` as egui reports them. The press is matched as
/// the dispatcher matches it ([`dispatch`]) on a layout about which nothing is known: what typing
/// has shown of the layout is left out, and so are double presses.
pub fn runs_a_shortcut(key: &str, code: &str, modifiers: Modifiers) -> bool {
    let physical_key = Key::from_name(code);
    let Some(key) = Key::from_name(key).or(physical_key) else { return false };
    let press = egui::Event::Key { key, physical_key, pressed: true, repeat: false, modifiers };
    let Some(input) = craft_keymap::KeyInput::from_egui(&press, crate::keymap::platform()) else { return false };
    let mirror = crate::shortcut_editor::current();
    craft_keymap::Layout::default().candidates(&input).iter().any(|c| mirror.keymap.lookup(c, false, &Active::NONE) != craft_keymap::Lookup::None)
}

/// Give a copy or cut chord held with more modifiers its key back when it has a shortcut of its own:
/// egui reports every Cmd+C and Cmd+X chord as Copy or Cut, Shift and Alt held too, so Cmd+Shift+C
/// (Align center) copied instead (#1940).
fn unclip_chords(i: &mut egui::InputState) {
    if !i.events.iter().any(|e| matches!(e, egui::Event::Copy | egui::Event::Cut)) {
        return;
    }
    let (held, shortcuts) = (i.modifiers, all_shortcuts());
    for e in &mut i.events {
        let (key, own) = match e {
            egui::Event::Copy => (Key::C, "edit.copy"),
            egui::Event::Cut => (Key::X, "edit.cut"),
            _ => continue,
        };
        if shortcuts.iter().any(|(sc, id, _)| sc.logical_key == key && *id != own && held.matches_exact(sc.modifiers)) {
            *e = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: held };
        }
    }
}

/// Drop each key press made with Ctrl and Alt held that is followed by the text it typed. Firefox
/// on Windows reports AltGr as Ctrl and Alt held, so AltGr+L on a Polish layout arrives as a
/// Ctrl+Alt+L press and then the text "ł", which the Type tool types. Without the press no command
/// chord runs for it: neither Cmd+Alt+L nor Cmd+L, which egui matches with the extra Alt held.
fn drop_typing_presses(i: &mut egui::InputState) {
    let next = i.events.iter().skip(1).map(Some).chain([None]);
    let typed: Vec<bool> = i
        .events
        .iter()
        .zip(next)
        .map(|(e, next)| {
            matches!(e, egui::Event::Key { pressed: true, modifiers, .. } if modifiers.ctrl && modifiers.alt)
                && matches!(next, Some(egui::Event::Text(t)) if !t.is_empty())
        })
        .collect();
    let mut typed = typed.into_iter();
    i.events.retain(|_| !typed.next().unwrap_or(false));
}

/// Keyboard pastes of something other than text. egui swallows a paste chord's key press and
/// sends a Paste event only when the clipboard holds text, so a paste key released without its
/// press (or a Paste event) reported was a paste of a bitmap or a PDF.
#[derive(Clone, Default)]
pub(crate) struct PasteChord {
    /// Paste keys whose press was reported (typed, not pasting).
    pressed: Vec<Key>,
    /// A Paste event came since the last paste key was released.
    pasted: bool,
}

impl PasteChord {
    /// Follow one frame's `events` → the modifiers of a paste egui sent no event for (the chord's,
    /// as its key is released).
    pub(crate) fn textless_paste(&mut self, events: &[egui::Event]) -> Option<Modifiers> {
        let mut fire = None;
        for e in events {
            match e {
                egui::Event::Paste(_) => self.pasted = true,
                // A release that comes while the window is away is never seen.
                egui::Event::WindowFocused(false) => *self = Self::default(),
                egui::Event::Key { key, pressed: true, .. } if PASTE_KEYS.contains(key) && !self.pressed.contains(key) => self.pressed.push(*key),
                egui::Event::Key { key, pressed: false, modifiers, .. } if PASTE_KEYS.contains(key) => {
                    match self.pressed.iter().position(|k| k == key) {
                        Some(i) => {
                            self.pressed.swap_remove(i);
                        }
                        // Unless a Paste event came (the guard takes it either way).
                        None if !std::mem::take(&mut self.pasted) => fire = Some(*modifiers),
                        None => {}
                    }
                }
                _ => {}
            }
        }
        fire
    }
}

/// End an IME composition outside the IME (a click away, the IME going away): the
/// marked text stays as typed, and the system IME is told to drop it.
pub(crate) fn keep_marked_text(app: &mut VectorcraftApp) {
    let Some(m) = app.ime_marked.take() else { return };
    app.ime_discard = true;
    if app.session.tool_composing() {
        // Errors are already reported by the engine's interaction.
        let _ = app.session.tool_text(&m, app.view_info());
    }
}

/// Cmd/Ctrl chords are shortcuts (Create Outlines, Bold, …): their letter is never typed into the
/// text if a host still emits `Text` for them (#2094). Alt is left to type: Option+key on macOS
/// and AltGr (Ctrl+Alt) on Windows keyboards type characters such as é, ø and @.
fn is_shortcut_chord(m: egui::Modifiers) -> bool {
    (m.command || m.ctrl) && !m.alt
}

enum Seq {
    Text(String),
    Preedit(String, Option<std::ops::Range<usize>>),
    Commit(String),
    Erase(ToolKey, Mods),
    /// An input method replaces text around the composition (upstream's character counts or
    /// the fork's exact UTF-8 byte counts), then commits.
    DeleteSurrounding(usize, usize, SurroundingUnit),
    // The actual pointer/key still belongs to its existing handler. Its position in this
    // sequence must nevertheless interrupt an old composition before later terminal input.
    InterruptPointer,
    InterruptNavigation,
    InterruptCommand,
    ResetCompleted(egui::ImeResetId),
}

/// Typed text and IME events, in the order they came, to the Type tool.
fn type_text(app: &mut VectorcraftApp, ctx: &egui::Context) -> bool {
    let view = app.view_info();
    // Switching apps mid-composition needs nothing: the macOS IME keeps its marked text and goes
    // on with it when the window comes back (measured with Kotoeri), as the tool does.
    let shortcut_mods = ctx.input(|i| is_shortcut_chord(i.modifiers));
    // A frame that began composing keeps its editing keys for the IME: a Backspace that empties the
    // marked text must not then delete the committed character before it.
    let was_composing = app.session.tool_composing();
    // Text, IME events and plain Backspace/Delete are taken together, in the order they arrived, so
    // a keyboard-hook IME (UniKey/OpenKey) whose tone key sends Backspace(s) and then re-injects
    // characters in one frame interleaves correctly. Taken as two passes (all text, then the keys)
    // the deletes land after the inserts and the diacritics are lost. Each handled event is removed
    // so `route_type_input` below does not process the same Backspace/Delete again.
    let seq = type_sequence(ctx);
    let viewport = ctx.viewport_id();
    let mut old_composition_retired = false;
    for e in seq {
        // Before the UI, a changed composition or guard is settled `pending`: a token bound here
        // could outlive a route this pass invalidates later (a palette/dialog/Home) and attach to
        // the stock descriptor. The post-UI finalizer binds the confirmed replacement.
        crate::ime::sync_incoming_before_ui(app, ctx);
        if let Seq::ResetCompleted(request) = &e {
            old_composition_retired |= crate::ime::complete(app, ctx, *request);
            continue;
        }
        if matches!(app.ime_compositions.get(&viewport), Some(crate::ime::IncomingComposition::Stale)) {
            match &e {
                // Accepted old payloads precede the fence even when their terminal arrives.
                // None of these events proves the local reset has reached its boundary.
                Seq::Commit(_) | Seq::DeleteSurrounding(..) | Seq::Preedit(..) => continue,
                _ => {}
            }
        }
        // A pointer/navigation/command notice has not reached its real handler yet. Text and
        // erases after it belong to the old caret, not to the destination it will establish.
        if crate::ime::deferred(app, ctx) {
            match &e {
                Seq::Text(_) | Seq::Erase(..) => continue,
                _ => {}
            }
        }
        let preedit_empty = match &e {
            Seq::Preedit(text, _) => Some(text.is_empty()),
            _ => None,
        };
        let terminal_commit = matches!(&e, Seq::Commit(_));
        let from_ime = matches!(&e, Seq::Preedit(..) | Seq::Commit(_) | Seq::DeleteSurrounding(..));
        // Errors are already reported by the engine's interaction (a failed preview keeps it).
        let result = apply_type_event(app, ctx, view, e, shortcut_mods, was_composing, &mut old_composition_retired);
        if result.is_ok() && from_ime {
            if terminal_commit {
                app.ime_compositions.remove(&viewport);
            } else {
                crate::ime::remember_incoming(app, ctx, preedit_empty);
            }
        }
    }
    old_composition_retired
}

/// Take this frame's typed text, IME events and plain Backspace/Delete, in the order they arrived.
fn type_sequence(ctx: &egui::Context) -> Vec<Seq> {
    let mut seq = vec![];
    ctx.input_mut(|i| {
        // Move the events out instead of cloning their strings: a keyboard hook that reinjects a
        // batch keeps the batch in `seq` only, not in both the input list and a second copy.
        let mut kept = Vec::with_capacity(i.events.len());
        // InputState's held flag describes the end of the batch. Recover its initial state from
        // the first transition, so a move before a later press cannot cancel an earlier commit.
        let mut primary_down = i
            .events
            .iter()
            .find_map(|e| match e {
                egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed, .. } => Some(!pressed),
                _ => None,
            })
            .unwrap_or_else(|| i.pointer.primary_down());
        for e in std::mem::take(&mut i.events) {
            match e {
                egui::Event::Text(t) => seq.push(Seq::Text(t)),
                egui::Event::Ime(egui::ImeEvent::Preedit { text, active_range_chars }) => {
                    seq.push(Seq::Preedit(text, active_range_chars));
                }
                egui::Event::Ime(egui::ImeEvent::Commit(t)) => seq.push(Seq::Commit(t)),
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars, after_chars }) => {
                    seq.push(Seq::DeleteSurrounding(before_chars, after_chars, SurroundingUnit::Chars));
                }
                egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes, after_bytes }) => {
                    seq.push(Seq::DeleteSurrounding(before_bytes, after_bytes, SurroundingUnit::Bytes));
                }
                egui::Event::ImeResetCompleted { request_id } => seq.push(Seq::ResetCompleted(request_id)),
                // X11/Wayland PRIMARY middle-click: text at the pointer, inserted as typed text (never
                // routed as a key or shortcut).
                egui::Event::MiddleClickPaste { text, .. } => seq.push(Seq::Text(text)),
                // Only unmodified Backspace/Delete interleave with injected text; word-delete and other
                // modified forms stay with `route_type_input`.
                egui::Event::Key { key: key @ (Key::Backspace | Key::Delete), pressed: true, modifiers: m, .. }
                    if !m.command && !m.alt && !m.ctrl =>
                {
                    let tk = if key == Key::Backspace { ToolKey::Backspace } else { ToolKey::Delete };
                    seq.push(Seq::Erase(tk, Mods { shift: m.shift, ..Mods::default() }));
                }
                e @ egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed, .. } => {
                    primary_down = pressed;
                    if pressed {
                        seq.push(Seq::InterruptPointer);
                    }
                    kept.push(e);
                }
                e @ egui::Event::PointerMoved(_) => {
                    if primary_down {
                        seq.push(Seq::InterruptPointer);
                    }
                    kept.push(e);
                }
                e @ egui::Event::Key { key, pressed: true, modifiers, .. }
                    if matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End)
                        || key == Key::A && modifiers.command =>
                {
                    seq.push(Seq::InterruptNavigation);
                    kept.push(e);
                }
                e @ egui::Event::Key { pressed: true, modifiers, .. } if is_shortcut_chord(modifiers) => {
                    seq.push(Seq::InterruptCommand);
                    kept.push(e);
                }
                e @ (egui::Event::Cut | egui::Event::Paste(_)) => {
                    seq.push(Seq::InterruptCommand);
                    kept.push(e);
                }
                other => kept.push(other),
            }
        }
        i.events = kept;
    });
    seq
}

/// Apply one sequence item to the active tool; the caller filters stale/deferred events and
/// turns a successful IME mutation into its receipt.
fn apply_type_event(
    app: &mut VectorcraftApp,
    ctx: &egui::Context,
    view: ViewInfo,
    e: Seq,
    shortcut_mods: bool,
    was_composing: bool,
    old_composition_retired: &mut bool,
) -> EngineResult<Vec<UiRequest>> {
    match e {
        Seq::InterruptPointer => {
            *old_composition_retired |= crate::ime::defer_interrupt(app, ctx);
            Ok(vec![])
        }
        Seq::InterruptNavigation => {
            // Active composition owns its navigation keys. Once cleared, application caret
            // navigation interrupts its still-pending terminal event, even in this batch.
            if !app.session.tool_composing() {
                *old_composition_retired |= crate::ime::defer_interrupt(app, ctx);
            }
            Ok(vec![])
        }
        Seq::InterruptCommand => {
            if crate::ime::blocked(app, ctx) {
                *old_composition_retired |= crate::ime::defer_interrupt(app, ctx);
            }
            Ok(vec![])
        }
        Seq::ResetCompleted(_) => Ok(vec![]),
        // Keys the IME passed through come as text only when nothing is marked. This is an
        // application edit (also a hook IME's reinjected characters): `Other` dominates.
        Seq::Text(t) if !app.session.tool_composing() && !shortcut_mods => {
            let target_before = crate::ime::target(&mut app.session, view);
            let before = crate::ime::stamp(&mut app.session, view);
            let r = app.session.tool_text(&t, view);
            if r.is_ok() {
                crate::ime::note_application_edit(app, ctx, view, target_before, before);
            }
            r
        }
        Seq::Text(_) => Ok(vec![]),
        // A pre-edit is not committed text: it changes neither the outgoing snapshot (the
        // engine query removes it) nor the cause.
        Seq::Preedit(text, active_range_chars) => {
            app.ime_marked = Some(text.clone()).filter(|t| !t.is_empty());
            app.session.tool_preedit(&text, active_range_chars, view)
        }
        // A bare line break confirms the composition (Enter): the marked text, not a newline.
        // It is an accepted IME commit of that marked text when the tool inserts the marked
        // text unchanged; a commit the tool rewrites (a quoted or filtered marked text) is an
        // application edit instead.
        Seq::Commit(t) if t == "\n" || t == "\r" => {
            let target_before = crate::ime::target(&mut app.session, view);
            let before = crate::ime::stamp(&mut app.session, view);
            let composing = app.session.tool_composing();
            let verbatim = app.session.tool_ime_session_matches(view)
                && match app.ime_marked.as_deref() {
                    Some(m) => app.session.tool_text_is_verbatim(m, view),
                    None => true,
                };
            keep_marked_text(app);
            if composing {
                if verbatim {
                    crate::ime::note_ime_apply(app, ctx, view, target_before, before);
                } else {
                    crate::ime::note_application_edit(app, ctx, view, target_before, before);
                }
            }
            Ok(vec![])
        }
        // An accepted IME commit: stamp the bounded committed context immediately before and
        // after, so only a real mutation (not a no-op, rejection or failure) earns the receipt.
        // A commit the tool rewrites (Typographer's Quotes, a filtered control character) is an
        // application edit and must never earn `InputMethod`.
        Seq::Commit(t) => {
            app.ime_marked = None;
            let target_before = crate::ime::target(&mut app.session, view);
            let before = crate::ime::stamp(&mut app.session, view);
            // A document edit since the composition means this commit starts a fresh typing
            // session: that reset is application-side, so it cannot earn `InputMethod`.
            let verbatim = app.session.tool_ime_session_matches(view) && app.session.tool_text_is_verbatim(&t, view);
            let r = app.session.tool_text(&t, view);
            if r.is_ok() {
                if verbatim {
                    crate::ime::note_ime_apply(app, ctx, view, target_before, before);
                } else {
                    crate::ime::note_application_edit(app, ctx, view, target_before, before);
                }
            }
            r
        }
        // While composing (or in a frame that began composing), editing keys belong to the IME.
        Seq::Erase(tk, m) if !was_composing && !app.session.tool_composing() => {
            // Mirror `route_type_input`/`text_key`: surface an erase error in the status bar
            // instead of discarding it, and report the successful edit as application-origin.
            let target_before = crate::ime::target(&mut app.session, view);
            let before = crate::ime::stamp(&mut app.session, view);
            let r = app.session.tool_key(tk, m, view);
            match &r {
                Err(e) => app.status(e.to_string()),
                Ok(_) => crate::ime::note_application_edit(app, ctx, view, target_before, before),
            }
            r
        }
        Seq::Erase(..) => Ok(vec![]),
        // Deleting around the composition starts the same typing session as the commit that
        // follows, so the replacement is one undo step; an error reaches the status bar. The
        // receipt covers only this accepted deletion.
        Seq::DeleteSurrounding(before, after, unit) => {
            let target_before = crate::ime::target(&mut app.session, view);
            let stamp_before = crate::ime::stamp(&mut app.session, view);
            let r = app.session.tool_ime_delete_surrounding(before, after, unit, view);
            match &r {
                Err(e) => app.status(e.to_string()),
                Ok(_) => crate::ime::note_ime_apply(app, ctx, view, target_before, stamp_before),
            }
            r
        }
    }
}

/// Process one frame's events against the keymap at most once per context, viewport and frame, so
/// reruns cannot fire a shortcut or tool key twice.
pub fn handle(app: &mut VectorcraftApp, ctx: &egui::Context) {
    // One UI pass takes a frame's events. A discarded rerun and a test helper that also calls
    // `logic` see the same events again; they must not fire a shortcut or tool key twice. The
    // key includes the context (a restarted host starts at frame 0) and the viewport (frame
    // numbers are per viewport), and remembers every key taken this host iteration: an
    // interleaved root/auxiliary pass cannot make an earlier frame look new again.
    let frame = (crate::graphics::context_tag(ctx), ctx.viewport_id(), ctx.cumulative_frame_nr_for(ctx.viewport_id()));
    if !crate::take_frame_once(&mut app.shortcuts_frames, frame) {
        return;
    }
    if !(ctx.input(|input| input.focused) || app.forwarded_frame && ctx.viewport_id() == egui::ViewportId::ROOT) {
        crate::ime::controls_without_editor(app, ctx);
        return;
    }
    // Taken before anything returns, so a press in a field or a dialog ends a pair of Escapes.
    let mut escape_pair = escape_press(ctx);
    // Followed before anything returns, so a key typed in a field isn't taken for a paste.
    let textless_paste = ctx.input(|i| app.paste_chord.textless_paste(&i.events));
    // A numeric field being scrubbed has the keyboard: Escape cancels the drag.
    if crate::scrub::phase(ctx) != crate::scrub::Phase::Idle {
        crate::ime::controls_without_editor(app, ctx);
        return;
    }
    if app.ui.dialog.is_some() || app.ui.palette_open {
        crate::ime::controls_without_editor(app, ctx);
        if app.ui.dialog.as_ref().is_some_and(crate::dialogs::revolve_gizmo::active) && crate::dialogs::revolve_gizmo::dragging(ctx) {
            // Escape first restores this rotation gesture. The settings stay open.
            return;
        }
        if crate::shortcut_editor::is_recording(app) {
            return;
        }
        // Escape closes a dropdown list or popover open in the dialog (each closes itself on it) and
        // leaves the dialog open.
        let popup = app.ui.dialog.as_ref().is_some_and(|d| egui::Popup::is_any_open(ctx) || crate::dialogs::brush_options::tips_open(d));
        if !popup && ctx.input(|i| i.key_pressed(Key::Escape)) {
            crate::dialogs::cancel(app);
            app.ui.palette_open = false;
        }
        return;
    }
    if app.ui.screen_mode == 3 {
        crate::ime::controls_without_editor(app, ctx);
        presentation_keys(app, ctx, escape_pair);
        return;
    }
    let typing = ctx.egui_wants_keyboard_input();
    // Escape closes Edit Toolbar's drawer first (egui drops a drag from it on Escape already).
    if app.ui.tools_drawer && !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        app.ui.tools_drawer = false;
    }
    let view = app.view_info();
    // While an IME composes, Enter and Escape are its own (confirm / cancel).
    let composing = app.session.tool_composing();
    // Tool keys first (Enter/Escape end paths; arrows change polygon sides while dragging).
    let busy = app.session.tool_busy();
    for (k, tk) in [(Key::Enter, ToolKey::Enter), (Key::Escape, ToolKey::Escape)] {
        if !typing && !composing && ctx.input(|i| i.key_pressed(k)) {
            // A chord a command is bound to (Cmd+Enter) runs only that command, in the shortcuts
            // below, also when the tool claims the key. While the Type tool edits text only Cmd
            // chords run one, and while another tool is busy none does.
            let text = app.session.tool_wants_text();
            if (text || !busy) && ctx.input(|i| runs_a_command(i, k, text)) {
                continue;
            }
            // A key the tool claims (Esc with a loaded place cursor) is only the tool's.
            let claimed = app.session.tool_claims_key(tk, view);
            let r = app.session.tool_key(tk, Mods::default(), view);
            // Enter also takes what the tool asks of the UI: Rotate, Scale, Reflect and Shear open
            // their dialog with it.
            if claimed || (k == Key::Enter && !busy) {
                crate::canvas::apply_requests(app, r);
            }
            if claimed {
                ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
            }
            if k == Key::Escape && !busy && !claimed {
                if app.session.active().is_some_and(|d| d.doc.pattern_edit.is_some()) {
                    let _ = app.run("object.pattern.done", json!({}));
                } else if app.session.active().is_some_and(|d| d.isolation.is_some()) {
                    let _ = app.run("object.exitIsolation", json!({}));
                } else if app.session.active().is_some_and(|d| d.global_edit.is_some()) {
                    let _ = app.run("select.globalEdit", json!({}));
                } else if app.ui.flyout.is_some() {
                    app.ui.flyout = None;
                } else if escape_pair.is_some_and(|second| escape_resets_rotation(app, ctx, second)) {
                    // Escape pressed twice resets a rotated view (with the Rotate View tool, once).
                    let _ = app.run("view.rotateReset", json!({}));
                    escape_pair = None;
                }
            }
            // An Escape that ended something can start a pair too. One that closed a popup cannot.
            if k == Key::Escape && escape_pair.is_some() && !egui::Popup::is_any_open(ctx) {
                let now = ctx.input(|i| i.time);
                ctx.data_mut(|d| d.insert_temp(escape_pair_id(), now));
            }
            if k == Key::Enter && !busy && !claimed {
                // Enter with a selection tool opens the Move dialog, as a double-click on its button
                // does.
                let tool = app.session.tool_id();
                if app.ui.dialog.is_none() && vectorcraft_tools::catalog::is_selection_tool(tool) {
                    // Nothing selected: it fails and nothing opens (the menu item is disabled then too).
                    let _ = crate::toolbar::open_options(app, tool);
                }
                // The Enter that opened a dialog isn't also its OK: dialogs take Enter, this frame too.
                if app.ui.dialog.is_some() {
                    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                }
            }
        }
    }
    if typing {
        crate::ime::controls_without_editor(app, ctx);
        return;
    }
    ctx.input_mut(unclip_chords);
    // Tab is ours when no field has the keyboard (it shows and hides the panels, or goes to the
    // Type tool): left to egui, it would also move the focus on to a field, and every key after it
    // would count as typing.
    if ctx.input(|i| i.key_pressed(Key::Tab)) {
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
    // While the Type tool edits text, a character typed with AltGr runs no command chord.
    if app.session.tool_wants_text() {
        ctx.input_mut(drop_typing_presses);
    }
    // Type tool editing: text, IME composition and editing keys go to the tool.
    if app.session.tool_wants_text() {
        // Route-level loss (no current target or caret geometry) retires a live guard before any
        // marker in this pass can be considered. The finalizer may re-arm on a later activation.
        crate::ime::suspend_if_route_lost(app, ctx);
        let old_composition_retired = type_text(app, ctx);
        // Keys of a frame that began composing are the IME's too (a Backspace that empties the
        // marked text must not delete the committed character before it).
        if composing && !old_composition_retired || app.session.tool_composing() {
            // The IME owns the keyboard until it commits: no editing keys, clipboard or shortcuts.
            ctx.input_mut(|i| {
                i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)))
            });
            return;
        }
        // A deferred pointer/navigation notice has not established its destination yet. Its
        // clipboard suffix must not cut or paste at the old caret; only the notice's own actual
        // handler (kept in the raw vector) may run.
        if crate::ime::deferred(app, ctx) {
            ctx.input_mut(|input| {
                input.events.retain(|event| match event {
                    egui::Event::Cut | egui::Event::Paste(_) => false,
                    egui::Event::Key { key: egui::Key::X | egui::Key::V, pressed: true, modifiers, .. }
                        if modifiers.command && !modifiers.shift && !modifiers.alt =>
                    {
                        false
                    }
                    _ => true,
                })
            });
        }
        // Editing keys with modifiers and the clipboard (Cmd+A is Select All's, below: the text).
        crate::panels::character::route_type_input(app, ctx);
        // Enter was already delivered above as ToolKey::Enter (newline). Only chords with the
        // command key run commands: the rest type or edit.
        let hits = dispatch(app, ctx, |hit| hit.trigger.chord.mods.cmd);
        for c in hits {
            run_hit(app, ctx, &c);
        }
        settings_on_a_mac(app, ctx, mac(ctx));
        return;
    }
    crate::ime::controls_without_editor(app, ctx);
    // A clipboard image (eframe's `clipboard` feature, 0.37): the host's text paste path never
    // carries it. With the app's rich-format `SystemClipboard`, it is only the signal that the
    // clipboard holds no text — the next tick invokes Paste, whose `adopt_system_clipboard` reads
    // the formats once (never paste twice). Without one (the web/simple host), turn the pixels
    // into a PNG flavour and paste that.
    let image_paste = app.services.system_clipboard.is_none();
    let mut pasted_image: Option<Vec<u8>> = None;
    let mut read_image_formats = false;
    // `MiddleClickPaste` is deliberately left in `i.events`: the Type tool above already took it,
    // and otherwise egui's text field under the pointer (focused or not) pastes it later in this
    // pass. It is not a shortcut.
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::PasteImage(image) => {
                if image_paste {
                    if pasted_image.is_none() {
                        let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                        let rendered = vectorcraft_render::Rendered {
                            width: u32::try_from(image.size[0]).unwrap_or(0),
                            height: u32::try_from(image.size[1]).unwrap_or(0),
                            pixels,
                        };
                        // A renderer failure leaves the paste as the no-op an empty flavour gets.
                        pasted_image = rendered.to_png().ok();
                    }
                } else {
                    read_image_formats = true;
                }
                false
            }
            _ => true,
        })
    });
    if read_image_formats {
        // A text paste in the same frame wins, as it does for a clipboard with text and a picture:
        // the paste chord path below reads the text. An image-only clipboard is read here.
        if pasted_image.is_none() && textless_paste.is_none() && !ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))) {
            // Remember the chord as it was when the image was pasted: the next `tick` runs in a
            // later input pass whose modifiers need not describe this paste. The request also
            // schedules that tick; nothing else may be coming to run it.
            app.clipboard_image_request = Some(ctx.input(|i| i.modifiers));
            ctx.request_repaint();
        }
    }
    if let Some(png) = pasted_image {
        let held = ctx.input(|i| i.modifiers);
        app.clipboard_in = Some(Flavour { mime: vectorcraft_engine::cmd::clipboard::PNG, data: png });
        crate::menus::invoke(app, paste_command(held), json!({}));
        return;
    }
    // Clipboard keys arrive as events, not key presses; the chord held says which paste.
    let mut clip = vec![];
    ctx.input_mut(|i| {
        let held = i.modifiers;
        i.events.retain(|e| {
            let (id, text) = match e {
                egui::Event::Copy => ("edit.copy", None),
                egui::Event::Cut => ("edit.cut", None),
                egui::Event::Paste(t) => (paste_command(held), Some(t.clone())),
                _ => return true,
            };
            clip.push((id, text));
            false
        })
    });
    // Only the system clipboard service reads what isn't text.
    if let Some(id) = textless_paste.map(paste_command)
        && app.services.system_clipboard.is_some()
    {
        clip.push((id, None));
    }
    for (id, text) in clip {
        app.clipboard_in = text.map(|t| Flavour { mime: TEXT, data: t.into_bytes() });
        crate::menus::invoke(app, id, json!({}));
    }
    if busy {
        // The arrows while dragging: a polygon's sides, a grid's rows (↑/↓) and columns (←/→)…
        let arrows =
            [(Key::ArrowUp, ToolKey::Up), (Key::ArrowDown, ToolKey::Down), (Key::ArrowLeft, ToolKey::Left), (Key::ArrowRight, ToolKey::Right)];
        for (k, tk) in arrows {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k)) {
                let _ = app.session.tool_key(tk, Mods::default(), view);
            }
        }
        // Digits (5 while dragging with the Perspective Selection tool), once per press.
        let digits: Vec<u8> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => digit_of(*key),
                    _ => None,
                })
                .collect()
        });
        for d in digits {
            let _ = app.session.tool_key(ToolKey::Digit(d), Mods::default(), view);
        }
        // The tool gets each letter it takes (E while dragging a selection marquee) once per press,
        // without Cmd, Ctrl or Alt. The key is the one egui reports, which on a layout without Latin
        // letters is the key at that position on a US layout.
        let letters: Vec<char> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. }
                        if !(modifiers.command || modifiers.alt || modifiers.ctrl) =>
                    {
                        letter_of(*key)
                    }
                    _ => None,
                })
                .collect()
        });
        for c in letters {
            if app.session.tool_claims_key(ToolKey::Letter(c), view) {
                let _ = app.session.tool_key(ToolKey::Letter(c), Mods::default(), view);
            }
        }
        return;
    }
    // Keys the active tool claims ahead of their shortcuts (the Gradient tool's selected stop:
    // Delete/Backspace remove it, ←/→ nudge it; the loaded place cursor: the arrows cycle files).
    let m = ctx.input(|i| i.modifiers);
    for (k, tk) in [
        (Key::Delete, ToolKey::Delete),
        (Key::Backspace, ToolKey::Backspace),
        (Key::ArrowLeft, ToolKey::Left),
        (Key::ArrowRight, ToolKey::Right),
        (Key::ArrowUp, ToolKey::Up),
        (Key::ArrowDown, ToolKey::Down),
    ] {
        if ctx.input(|i| i.key_pressed(k)) && app.session.tool_claims_key(tk, view) && ctx.input_mut(|i| i.consume_key(m, k)) {
            let r = app.session.tool_key(tk, crate::canvas::mods(m, false), view);
            // Keyboard actions can change the Artboard tool's active board too. Keep the
            // navigator aligned with its valid survivor, as pointer dispatch does.
            if r.is_ok()
                && app.session.tool_id() == "artboard"
                && let Some(i) = app.session.tool_options()["active"].as_u64().and_then(|i| usize::try_from(i).ok())
                && app.session.active().is_some_and(|d| i < d.doc.artboards.len())
                && let Some(v) = app.view_mut()
            {
                v.artboard = i;
            }
            crate::canvas::apply_requests(app, r);
            return;
        }
    }
    // Digits the active tool or the perspective grid takes (1–4 pick the plane while it shows).
    if !(m.command || m.alt || m.ctrl) {
        let digits: Vec<(u8, Key)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => digit_of(*key).map(|d| (d, *key)),
                    _ => None,
                })
                .collect()
        });
        for (d, k) in digits {
            if app.session.tool_claims_key(ToolKey::Digit(d), view) && ctx.input_mut(|i| i.consume_key(m, k)) {
                // The digit's text is the key's too: no single-key shortcut sees it.
                let text = d.to_string();
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if *t == text)));
                let r = app.session.tool_key(ToolKey::Digit(d), crate::canvas::mods(m, false), view);
                crate::canvas::apply_requests(app, r);
                return;
            }
        }
    }
    // `[` and `]` the active tool takes (the Blob Brush's and the Eraser's size, the Symbolism
    // tools' diameter, with Shift their intensity). Read as typed, so
    // layouts where a bracket takes Option or AltGr count too; `{` and `}` are Shift's.
    if !(m.command || m.ctrl) {
        let typed: Vec<(String, ToolKey, bool)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => match t.as_str() {
                        "[" => Some((t.clone(), ToolKey::BracketLeft, false)),
                        "]" => Some((t.clone(), ToolKey::BracketRight, false)),
                        "{" => Some((t.clone(), ToolKey::BracketLeft, true)),
                        "}" => Some((t.clone(), ToolKey::BracketRight, true)),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        });
        let mut taken = false;
        for (text, key, shift) in typed {
            if app.session.tool_claims_key(key, view) {
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if *t == text)));
                let r = app.session.tool_key(key, Mods { shift, ..Mods::default() }, view);
                crate::canvas::apply_requests(app, r);
                taken = true;
            }
        }
        if taken {
            return;
        }
    }
    // Command, panel and tool shortcuts. Keys that type a character without Cmd/Alt/Ctrl are
    // handled below as text (tool shortcuts, X, D, /, `,` and `.`).
    let hits = dispatch(app, ctx, |hit| !(hit.trigger.chord.is_plain() && types_text(hit.trigger.chord.key)));
    let ran = !hits.is_empty();
    for c in hits {
        run_hit(app, ctx, &c);
    }
    if settings_on_a_mac(app, ctx, mac(ctx)) || ran {
        return;
    }
    // Alt+arrows on selected type objects step their spacing as the Type tool does (Cmd/Ctrl:
    // five steps); Alt+arrow copies and nudges every other selection.
    let m = ctx.input(|i| i.modifiers);
    if m.alt
        && app.session.active().is_some_and(|st| {
            st.selection.guides.is_empty()
                && !st.selection.objects.is_empty()
                && st.selection.objects.iter().all(|id| matches!(st.doc.node(*id).map(|n| &n.kind), Some(vectorcraft_doc::NodeKind::Text(_))))
        })
    {
        let keys = [(Key::ArrowLeft, ToolKey::Left), (Key::ArrowRight, ToolKey::Right), (Key::ArrowUp, ToolKey::Up), (Key::ArrowDown, ToolKey::Down)];
        for (key, tool_key) in keys {
            if ctx.input_mut(|i| i.consume_key(m, key)) {
                if let Some((attribute, by)) = vectorcraft_tools::text::step_key(tool_key, m.shift, false) {
                    let by = if m.command { by * 5.0 } else { by };
                    let _ = app.run("type.step", json!({"attribute": attribute, "by": by}));
                }
                return;
            }
        }
    }
    // Arrow nudges (Shift = ×10, Alt = copy).
    let arrows = [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, -1.0), (Key::ArrowDown, 0.0, 1.0)];
    for (k, dx, dy) in arrows {
        let m = ctx.input(|i| i.modifiers);
        if ctx.input_mut(|i| i.consume_key(m, k)) && app.session.active().is_some_and(|d| d.selection.has_objects_or_guides()) {
            let _ = app.run("object.nudge", json!({"dx": dx, "dy": dy, "big": m.shift, "copy": m.alt}));
        }
    }
    // Delete / Backspace clear the selection.
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) && app.session.active().is_some_and(|d| d.selection.has_objects_or_guides())
    {
        let _ = app.run("edit.clear", json!({}));
    }
    // Single-key tool shortcuts (no Cmd/Ctrl/Alt). With a layout without Latin letters (Persian,
    // Arabic, Russian, Greek…) the key's position stands in for what it types (#793).
    let events: Vec<(String, Modifiers)> = ctx.input(|i| {
        let mut physical = None;
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key { physical_key, pressed: true, .. } => {
                    physical = *physical_key;
                    None
                }
                egui::Event::Text(t) => Some((latin(t, physical), i.modifiers)),
                _ => None,
            })
            .collect()
    });
    let keymap = crate::shortcut_editor::current();
    for (text, m) in events {
        if m.command || m.alt || m.ctrl {
            continue;
        }
        let upper = text.to_uppercase();
        let key = if m.shift && text.chars().all(|c| c.is_alphabetic()) { format!("Shift+{upper}") } else { upper.clone() };
        // A shifted punctuation character can also be written with its Shift (the Curvature tool's
        // Shift+~ arrives as `~`).
        let shifted = m.shift.then(|| format!("Shift+{text}"));
        let hits: Vec<CommandRef> = [Some(key), Some(text), shifted]
            .into_iter()
            .flatten()
            .filter_map(|t| crate::keymap::vc_chord(&t).and_then(|c| crate::keymap::lookup(&keymap.keymap, &c)))
            .collect();
        // Single-key command shortcuts (X, Shift+X, D, /, Shift+D, F, Shift+F by default) before
        // tools.
        let Some(hit) = hits.iter().find(|c| !c.id.starts_with("tool:")).or(hits.first()) else { continue };
        match hit.id.strip_prefix("tool:") {
            Some(tool) => app.select_tool(tool),
            None if hit.params.is_null() => {
                let _ = app.run(&hit.id, json!({}));
            }
            None => crate::menus::invoke(app, &hit.id, hit.params.clone()),
        }
    }
}

/// Runs this frame's key presses against the keymap in effect; `accept` declines a hit, leaving
/// its press (and the text it typed) to what follows.
fn dispatch(app: &mut VectorcraftApp, ctx: &egui::Context, accept: impl FnMut(&Hit) -> bool) -> Vec<CommandRef> {
    let mirror = crate::shortcut_editor::current();
    let dispatcher = &mut app.dispatcher;
    ctx.input_mut(|i| {
        // Shift+`=` types `+`: as the Plus key it also reaches a `=` chord held with Shift
        // (Cmd+Shift+= zooms in as Cmd+= does).
        for e in &mut i.events {
            if let egui::Event::Key { key, modifiers, .. } = e
                && *key == Key::Equals
                && modifiers.shift
            {
                *key = Key::Plus;
            }
        }
        dispatcher.dispatch_filtered(i, &mirror.keymap, &Active::NONE, accept).into_iter().map(|h| h.command).collect()
    })
}

/// Runs what a shortcut is bound to: a tool, a panel (`window.panel {panel}`) or a command.
fn run_hit(app: &mut VectorcraftApp, ctx: &egui::Context, c: &CommandRef) {
    if let Some(tool) = c.id.strip_prefix("tool:") {
        app.select_tool(tool);
    } else if c.id == "select.all" && crate::panels::artboards::selects_all(app, ctx) {
        // With the Artboard tool, Select All's keys after a click on an Artboards panel row select
        // every artboard instead of the art.
        crate::panels::artboards::select_all(app);
    } else {
        let p = if c.params.is_null() { json!({}) } else { c.params.clone() };
        crate::menus::invoke(app, &c.id, p);
    }
}

/// Keys that type a character: letters, digits and punctuation (not Space).
fn types_text(key: craft_keymap::Key) -> bool {
    key.name().chars().count() == 1
}

/// Whether the app runs on a Mac (on the web, in a browser on a Mac).
fn mac(ctx: &egui::Context) -> bool {
    ctx.os() == egui::os::OperatingSystem::Mac
}

/// On a Mac, Cmd+, opens Settings as in every Mac app (#663), beside Preferences' own shortcut,
/// when no shortcut took the press this frame. Runs after [`dispatch`]. `true` when it ran.
fn settings_on_a_mac(app: &mut VectorcraftApp, ctx: &egui::Context, mac: bool) -> bool {
    if !mac {
        return false;
    }
    let pressed = ctx.input_mut(|i| {
        let at = i.events.iter().position(
            |e| matches!(e, egui::Event::Key { key: Key::Comma, pressed: true, modifiers, .. } if modifiers.matches_exact(Modifiers::COMMAND)),
        );
        at.map(|at| i.events.remove(at)).is_some()
    });
    if pressed {
        crate::menus::invoke(app, "edit.preferences", json!({}));
    }
    pressed
}

/// The second press of Escape pressed twice comes less than this many seconds after the first.
const ESCAPE_PAIR_SECONDS: f64 = 1.0;

/// The key of the last counted Escape's time in egui's temporary data.
fn escape_pair_id() -> egui::Id {
    egui::Id::unique("escape-pair")
}

/// Reads this frame's presses for Escape pressed twice. `None` when the frame has no Escape press, or
/// has another key or pointer button press as well (a key repeat counts as another press, and the
/// modifier keys do not count). Otherwise, whether the last counted Escape came less than
/// [`ESCAPE_PAIR_SECONDS`] before. A press of Escape, another key or a pointer button removes the
/// last counted Escape's time.
fn escape_press(ctx: &egui::Context) -> Option<bool> {
    let (escape, other, now) = ctx.input(|i| {
        let (mut escape, mut other) = (false, false);
        for e in &i.events {
            match e {
                egui::Event::Key { key: Key::Escape, pressed: true, repeat: false, .. } => escape = true,
                egui::Event::Key { key, pressed: true, .. } if !is_modifier(*key) => other = true,
                egui::Event::PointerButton { pressed: true, .. } => other = true,
                _ => {}
            }
        }
        (escape, other, i.time)
    });
    if !escape && !other {
        return None;
    }
    let counted = ctx.data_mut(|d| d.remove_temp::<f64>(escape_pair_id()));
    (escape && !other).then(|| counted.is_some_and(|t| now - t < ESCAPE_PAIR_SECONDS))
}

/// Does this Escape reset the view's rotation? It does when the view is rotated, no popup is open,
/// the ruler origin is not being dragged, and the press is the second of a pair (`second`) or the
/// Rotate View tool is chosen.
fn escape_resets_rotation(app: &VectorcraftApp, ctx: &egui::Context, second: bool) -> bool {
    app.view().is_some_and(|v| v.rotation != 0.0)
        && (second || app.session.tool_id() == "rotateView")
        && !egui::Popup::is_any_open(ctx)
        && !crate::canvas::ruler_origin_dragging(ctx)
}

/// The keys in Presentation Mode, where nothing is edited: Escape leaves the mode, and ← and → go to
/// the previous and the next artboard ([`presentation_step`]). Every other key press, the text it
/// types and the clipboard keys are removed from the input, so no shortcut, tool or field acts on
/// them. The macOS menu bar's key equivalents arrive as key presses too ([`crate::native_menu`]).
fn presentation_keys(app: &mut VectorcraftApp, ctx: &egui::Context, escape_pair: Option<bool>) {
    let (mut leave, mut steps) = (false, vec![]);
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key { key: Key::Escape, pressed: true, .. } => {
                leave = true;
                false
            }
            egui::Event::Key { key: Key::ArrowLeft, pressed: true, .. } => {
                steps.push(false);
                false
            }
            egui::Event::Key { key: Key::ArrowRight, pressed: true, .. } => {
                steps.push(true);
                false
            }
            egui::Event::Key { pressed: true, .. }
            | egui::Event::Text(_)
            | egui::Event::Ime(_)
            | egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Paste(_)
            | egui::Event::PasteImage(_)
            | egui::Event::MiddleClickPaste { .. } => false,
            _ => true,
        })
    });
    for next in steps {
        presentation_step(app, next);
    }
    if leave {
        let _ = app.run("view.presentation", json!({}));
        // The Escape that left the mode can start a pair, as in `handle`.
        if escape_pair.is_some() && !egui::Popup::is_any_open(ctx) {
            let now = ctx.input(|i| i.time);
            ctx.data_mut(|d| d.insert_temp(escape_pair_id(), now));
        }
    }
}

/// Go to the next artboard (`next`) or the previous one with `view.goToArtboard`, as the status bar
/// navigator's Next and Previous buttons do, for the arrow keys and a click in Presentation Mode.
/// On the last artboard a step forward leaves the view as it is, and so does a step back on the
/// first.
pub(crate) fn presentation_step(app: &mut VectorcraftApp, next: bool) {
    let n = app.session.active().map_or(0, |d| d.doc.artboards.len());
    let at = crate::panels::artboards::selected(app, n);
    let to = if next { (at + 1 < n).then_some("next") } else { (at > 0).then_some("previous") };
    if let Some(to) = to
        && let Err(e) = app.run("view.goToArtboard", json!({ "index": to }))
    {
        app.status(e);
    }
}

/// What a single-key shortcut reads for `text`, typed by the key at `physical`: the text itself,
/// or, when it holds no ASCII character (a layout without Latin letters), what that key types on a
/// US layout, so V selects the Selection tool whatever the V key types.
fn latin(text: &str, physical: Option<Key>) -> String {
    match physical {
        Some(k) if !text.is_ascii() => k.symbol_or_name().to_lowercase(),
        _ => text.to_string(),
    }
}

/// The digit a number-row or keypad key types.
fn digit_of(k: Key) -> Option<u8> {
    const DIGITS: [Key; 10] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    DIGITS.iter().position(|d| *d == k).map(|i| i as u8)
}

/// The lowercase letter a letter key A–Z stands for.
fn letter_of(k: Key) -> Option<char> {
    let mut name = k.name().chars();
    match (name.next(), name.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
