//! The "Import web page" dialog: URL, viewport preset, full page, and under
//! "Advanced" a CSS selector (and auto layout inference when available).
//! While the import runs the dialog shows the live progress; closing it
//! (Cancel, Escape) drops the job, which kills the browser.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::{
    CancelGuard, Failure, ImportRequest, Outcome, Preset, ProgressState, Stage, UNSAVED_DOC,
    assets_dir_for, dialog_agent, insert_page, spawn, viewport,
};
use crate::editor_state::EditorState;
use crate::theme::{Palette, PaletteExt, metrics};
use crate::workspace::{Workspace, WorkspaceRef, log};

/// Whether pen-import can infer auto layout (shows the switch).
pub const INFER_LAYOUT_SUPPORTED: bool = super::INFER_LAYOUT_SUPPORTED;

enum Phase {
    Form,
    Running(ProgressState),
    Failed(Failure),
}

pub struct WebImportDialog {
    ws: WeakEntity<Workspace>,
    doc: WeakEntity<EditorState>,
    url: Entity<InputState>,
    custom_width: Entity<InputState>,
    selector: Entity<InputState>,
    preset: Preset,
    full_page: bool,
    infer_layout: bool,
    advanced: bool,
    /// Validation message under the form.
    error: Option<String>,
    phase: Phase,
    /// Dropping it cancels the running import.
    cancel: Option<CancelGuard>,
    /// Holds the focus while the form is hidden (progress), so Escape
    /// still reaches the dialog.
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

/// Opens the dialog for the focused document.
pub fn open(window: &mut Window, cx: &mut App) {
    let Some(ws) = cx.try_global::<WorkspaceRef>().map(|w| w.0.clone()) else {
        return;
    };
    let Some(doc) = ws.upgrade().and_then(|w| w.read(cx).focused_doc()) else {
        window.push_notification("Open a document to import a web page into it.", cx);
        return;
    };
    let view = cx.new(|cx| WebImportDialog::new(ws, doc.downgrade(), window, cx));
    let focus = view.read(cx).url.clone();
    window.defer(cx, move |window, cx| {
        focus.update(cx, |s, cx| s.focus(window, cx));
    });
    window.open_dialog(cx, move |d, _, _| {
        let ok = view.downgrade();
        let cancel = view.downgrade();
        d.title(div().text_size(px(16.)).child("Import web page"))
            .width(px(460.))
            .overlay_closable(false)
            // Enter (the dialog's Confirm) starts the import instead of
            // closing the dialog, which would cancel it.
            .on_ok(move |_, window, cx| {
                let _ = ok.update(cx, |v, cx| v.start(window, cx));
                false
            })
            // Escape and the close button: stop the browser right away
            // (the view itself may outlive the closing animation).
            .on_cancel(move |_, _, cx| {
                let _ = cancel.update(cx, |v, _| v.stop());
                true
            })
            .child(view.clone())
    });
}

impl WebImportDialog {
    fn new(
        ws: WeakEntity<Workspace>,
        doc: WeakEntity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com"));
        let custom_width = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Width")
                .default_value("1280")
        });
        let selector =
            cx.new(|cx| InputState::new(window, cx).placeholder("Optional, e.g. main or #pricing"));
        let mut subs = Vec::new();
        for input in [&url, &custom_width, &selector] {
            subs.push(cx.subscribe_in(
                input,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { .. } => this.start(window, cx),
                    InputEvent::Change if this.error.take().is_some() => cx.notify(),
                    _ => {}
                },
            ));
        }
        Self {
            ws,
            doc,
            url,
            custom_width,
            selector,
            preset: Preset::Desktop,
            full_page: true,
            infer_layout: false,
            advanced: false,
            error: None,
            phase: Phase::Form,
            cancel: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn running(&self) -> bool {
        matches!(self.phase, Phase::Running(_))
    }

    /// Validates the form and starts the import.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        let url = match pen_mcp::normalize_web_url(&self.url.read(cx).value()) {
            Ok(u) => u,
            Err(e) => return self.invalid(capitalize(&e), cx),
        };
        let (width, height) = match viewport(self.preset, &self.custom_width.read(cx).value()) {
            Ok(v) => v,
            Err(e) => return self.invalid(e, cx),
        };
        let Some(doc) = self.doc.upgrade() else {
            return self.invalid("The document was closed.".into(), cx);
        };
        // The form shows the "save first" notice with a Save As button.
        let Some(path) = doc.read(cx).path.clone() else {
            return;
        };
        let selector = self.selector.read(cx).value().trim().to_string();
        let mut req = ImportRequest::new(url.clone());
        req.width = width;
        req.height = height;
        req.full_page = self.full_page;
        req.selector = (!selector.is_empty()).then_some(selector);
        req.infer_layout = INFER_LAYOUT_SUPPORTED && self.infer_layout;
        req.assets_dir = assets_dir_for(&path);
        // Show the normalized address.
        self.url
            .update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        log(&format!(
            "web import: {url} ({width}x{height}, full page: {})",
            req.full_page
        ));
        let job = spawn(req);
        self.cancel = Some(job.cancel);
        self.error = None;
        self.phase = Phase::Running(ProgressState::new(Instant::now()));
        window.focus(&self.focus, cx);
        cx.notify();
        let mut progress = job.progress;
        let mut result = job.result;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let mut stages = Vec::new();
                while let Ok(s) = progress.try_recv() {
                    stages.push(s);
                }
                let done = match result.try_recv() {
                    Ok(Some(r)) => Some(r),
                    Ok(None) => None,
                    Err(_) => Some(Err(Failure::Cancelled)),
                };
                let alive = this.update_in(cx, |this, window, cx| {
                    if let Phase::Running(p) = &mut this.phase {
                        for s in stages {
                            p.advance(s);
                        }
                    }
                    if let Some(r) = done {
                        this.finish(r, window, cx);
                    }
                    cx.notify();
                });
                // Stop when the dialog is gone or the import ended.
                if alive.is_err() || !matches!(this.update(cx, |t, _| t.running()), Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn invalid(&mut self, msg: String, cx: &mut Context<Self>) {
        self.error = Some(msg);
        cx.notify();
    }

    /// Stops a running import (the browser is killed).
    fn stop(&mut self) {
        if let Some(mut c) = self.cancel.take() {
            c.cancel();
            log("web import: cancelled");
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        window.close_dialog(cx);
    }

    fn finish(&mut self, r: Result<Outcome, Failure>, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel = None;
        let outcome = match r {
            Ok(o) => o,
            Err(Failure::Cancelled) => {
                self.phase = Phase::Form;
                return;
            }
            Err(f) => {
                log(&format!("web import: {}", f.message()));
                self.phase = Phase::Failed(f);
                return;
            }
        };
        if let Phase::Running(p) = &mut self.phase {
            p.advance(Stage::Inserting);
        }
        let Some(doc) = self.doc.upgrade() else {
            self.phase = Phase::Failed(Failure::Document("The document was closed.".into()));
            return;
        };
        let view = self
            .ws
            .upgrade()
            .and_then(|w| w.read(cx).visible_world(&doc, cx))
            .unwrap_or(pen_render::Rect::new(0.0, 0.0, 1.0, 1.0));
        let frame = outcome.frame();
        let agent = dialog_agent();
        let mut engine = pen_script::ScriptEngine::new();
        let inserted = doc.update(cx, |s, cx| {
            insert_page(s, &mut engine, &agent, &frame, view, cx)
        });
        match inserted {
            Ok(id) => {
                log(&format!(
                    "web import: inserted {id} ({} warnings, {} assets)",
                    outcome.warnings.len(),
                    outcome.assets_written.len()
                ));
                window.close_dialog(cx);
                notify_done(&outcome, window, cx);
            }
            Err(e) => {
                log(&format!("web import: insert failed: {e}"));
                self.phase = Phase::Failed(Failure::Document(e));
            }
        }
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(ws), Some(doc)) = (self.ws.upgrade(), self.doc.upgrade()) else {
            return;
        };
        ws.update(cx, |w, cx| w.save_as(&doc, window, cx));
    }
}

/// Success toast, plus the warnings (if any) in a second one.
fn notify_done(o: &Outcome, window: &mut Window, cx: &mut App) {
    let name = if o.title.trim().is_empty() {
        "the page".to_string()
    } else {
        format!("“{}”", o.title.trim())
    };
    window.push_notification(Notification::success(format!("Imported {name}.")), cx);
    if !o.warnings.is_empty() {
        let mut text: Vec<String> = o.warnings.iter().take(4).cloned().collect();
        if o.warnings.len() > 4 {
            text.push(format!("… and {} more.", o.warnings.len() - 4));
        }
        window.push_notification(
            Notification::warning(text.join("\n")).title(format!(
                "{} import warning{}",
                o.warnings.len(),
                if o.warnings.len() == 1 { "" } else { "s" }
            )),
            cx,
        );
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => {
            let mut t: String = f.to_uppercase().collect();
            t.push_str(c.as_str());
            if !t.ends_with('.') {
                t.push('.');
            }
            t
        }
        None => String::new(),
    }
}

fn button(id: &'static str, label: &'static str, primary: bool, p: &Palette) -> Stateful<Div> {
    let (bg, fg, input, hover) = (p.primary, p.primary_fg, p.input, p.hover);
    let text = p.fg;
    div()
        .id(id)
        .flex_none()
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(metrics::RADIUS_MD))
        .text_size(px(metrics::TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.bg(bg).text_color(fg).hover(|s| s.opacity(0.9))
            } else {
                d.border_1()
                    .border_color(input)
                    .text_color(text)
                    .hover(move |s| s.bg(hover))
            }
        })
        .child(label)
}

fn label(text: &'static str, p: &Palette) -> Div {
    div()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.fg)
        .child(text)
}

impl Render for WebImportDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let body = match &self.phase {
            Phase::Form => self.render_form(&p, cx).into_any_element(),
            Phase::Running(prog) => self.render_progress(prog, &p, cx).into_any_element(),
            Phase::Failed(f) => self.render_failure(f, &p, cx).into_any_element(),
        };
        div()
            .id("web-import")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap(px(14.))
            .text_size(px(13.))
            .child(body)
    }
}

impl WebImportDialog {
    fn render_form(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = p.fg_muted;
        let unsaved = self
            .doc
            .upgrade()
            .is_some_and(|d| d.read(cx).path.is_none());
        let seg_bg = p.control;
        let seg = |preset: Preset, active: bool| {
            let sub = preset
                .size()
                .map(|(w, _)| format!("{w}"))
                .unwrap_or_else(|| "…".into());
            div()
                .id(SharedString::from(format!("web-preset-{}", preset.label())))
                .flex_1()
                .h(px(40.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(if active { p.fg } else { p.fg_muted })
                .when(active, |d| d.bg(p.segment_active).shadow_xs())
                .when(!active, |d| d.hover(|s| s.text_color(p.fg)))
                .child(div().font_weight(FontWeight::MEDIUM).child(preset.label()))
                .child(div().text_size(px(10.)).text_color(muted).child(sub))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.preset = preset;
                    this.error = None;
                    cx.notify();
                }))
        };
        let presets = div()
            .flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .bg(seg_bg)
            .children(Preset::ALL.map(|pr| seg(pr, pr == self.preset)));
        let switch_row = |text: &'static str, hint: &'static str, sw: Switch| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(label(text, p))
                        .child(div().text_size(px(11.)).text_color(muted).child(hint)),
                )
                .child(sw.small())
        };
        let advanced = self.advanced;
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(div().text_size(px(12.)).text_color(muted).child(
                "OpenPen opens the page in a private browser window that is not shown, \
                         then adds it to the canvas as a frame of editable layers.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("URL", p))
                    .child(Input::new(&self.url)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("Viewport", p))
                    .child(presets)
                    .when(self.preset == Preset::Custom, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().w(px(120.)).child(Input::new(&self.custom_width)))
                                .child(div().text_size(px(12.)).text_color(muted).child("px wide")),
                        )
                    }),
            )
            .child(switch_row(
                "Full page",
                "Off: only the first screen of the page.",
                Switch::new("web-full-page")
                    .checked(self.full_page)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.full_page = *v;
                        cx.notify();
                    })),
            ))
            .child(
                div()
                    .id("web-advanced")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(muted)
                    .hover(|s| s.text_color(p.fg))
                    .child(
                        Icon::new(if advanced {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(12.)),
                    )
                    .child("Advanced")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advanced = !this.advanced;
                        cx.notify();
                    })),
            )
            .when(advanced, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label("CSS selector", p))
                        .child(Input::new(&self.selector))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(muted)
                                .child("Import a single element instead of the whole page."),
                        ),
                )
                .when(INFER_LAYOUT_SUPPORTED, |d| {
                    d.child(switch_row(
                        "Infer auto layout",
                        "Turn flex rows and columns into auto layout frames.",
                        Switch::new("web-infer-layout")
                            .checked(self.infer_layout)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.infer_layout = *v;
                                cx.notify();
                            })),
                    ))
                })
            })
            .when(unsaved, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .p(px(10.))
                        .rounded(px(metrics::RADIUS_MD))
                        .bg(p.control)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .text_color(p.fg)
                                .child(UNSAVED_DOC),
                        )
                        .child(
                            button("web-save-as", "Save As…", false, p).on_click(
                                cx.listener(|this, _, window, cx| this.save_as(window, cx)),
                            ),
                        ),
                )
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_size(px(12.)).text_color(p.danger).child(e))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("web-cancel", "Cancel", false, p)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        button("web-import", "Import", true, p)
                            .when(unsaved, |d| d.opacity(0.5))
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    ),
            )
    }

    fn render_progress(
        &self,
        prog: &ProgressState,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let url = self.url.read(cx).value().to_string();
        let secs = prog.elapsed(Instant::now()).as_secs();
        let frac = prog.fraction();
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(p.fg_muted)
                    .truncate()
                    .child(url),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(prog.label()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(p.fg_muted)
                            .child(format!("{secs} s")),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(6.))
                    .rounded_full()
                    .bg(p.control)
                    .child(div().h_full().rounded_full().bg(p.fg).w(relative(frac))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(p.fg_muted)
                    .child("Large pages can take a minute. Cancel stops the browser."),
            )
            .child(
                div().flex().justify_end().child(
                    button("web-stop", "Cancel", false, p)
                        .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                ),
            )
    }

    fn render_failure(&self, f: &Failure, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(
                        div()
                            .mt(px(1.))
                            .text_color(p.danger)
                            .child(Icon::new(IconName::CircleX).size(px(16.))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(f.title()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(p.fg_muted)
                                    .child(f.detail()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("web-close", "Close", false, p)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(button("web-back", "Back", true, p).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.phase = Phase::Form;
                            cx.notify();
                        },
                    ))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::capitalize;

    #[test]
    fn messages_are_sentences() {
        assert_eq!(capitalize("enter a url"), "Enter a url.");
        assert_eq!(capitalize("done."), "Done.");
        assert_eq!(capitalize("bad (x)"), "Bad (x).");
        assert_eq!(capitalize(""), "");
    }
}
