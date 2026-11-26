use crate::{
    capture::microphones,
    config::{Config, Insertion},
    dictation,
    pipeline::Pipeline,
    platform::{self, credential},
};
use anyhow::{Context, Result};
use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, WidgetInfo, WidgetType, pos2,
    vec2,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// How long closing the window waits for the worker to stop. A paste in
/// progress restores the previous clipboard text within about 0.4 s.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

const LOGO: &[u8] = include_bytes!("../assets/dictly-logo.png");

const BACKGROUND: Color32 = Color32::from_rgb(244, 246, 251);
const FIELD: Color32 = Color32::from_rgb(250, 251, 253);
const BORDER: Color32 = Color32::from_rgb(226, 232, 240);
const KEYCAP: Color32 = Color32::from_rgb(246, 247, 250);
const SWITCH_OFF: Color32 = Color32::from_rgb(203, 213, 225);
const TEXT: Color32 = Color32::from_rgb(15, 23, 42);
const MUTED: Color32 = Color32::from_rgb(100, 116, 139);
const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
const ACCENT_END: Color32 = Color32::from_rgb(139, 92, 246);
const DANGER: Color32 = Color32::from_rgb(220, 38, 38);
const DANGER_END: Color32 = Color32::from_rgb(225, 29, 72);
const SUCCESS: Color32 = Color32::from_rgb(22, 163, 74);

pub fn run(config_path: Option<PathBuf>) -> Result<()> {
    let _instance = platform::SingleInstance::acquire()?;
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("config.toml")));
    // Settings are saved where they were loaded from, else beside the executable.
    let save_path = config_path.clone().or_else(|| beside_exe.clone());
    let path = config_path.or_else(|| beside_exe.filter(|path| path.exists()));
    let config = Config::load(path.as_deref());
    // A saved key is offered only to the provider origin it was saved for; a
    // configuration that points elsewhere starts with an empty key field.
    let shown_url = config.as_ref().map_or_else(
        |_| Config::default().base_url,
        |config| config.base_url.clone(),
    );
    let saved_key = match credential::origin(shown_url.trim()) {
        Some(origin) => credential::load(&origin),
        None => Ok(None),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dictly")
            .with_inner_size([620.0, 820.0])
            .with_min_inner_size([440.0, 540.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(LOGO).expect("embedded logo is a valid PNG"),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "Dictly",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Desktop::new(
                config,
                path,
                save_path,
                saved_key,
                &cc.egui_ctx,
            )))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Cannot open Dictly desktop window: {error}"))
}

enum Event {
    Status(String),
    Finished(Result<(), String>),
}

struct Worker {
    stop: Arc<AtomicBool>,
    events: mpsc::Receiver<Event>,
    thread: thread::JoinHandle<()>,
}

struct Desktop {
    config: Config,
    source: String,
    save_path: Option<PathBuf>,
    language: String,
    api_key: String,
    authenticated: bool,
    /// Store the typed key in the OS key store when enabling.
    remember_key: bool,
    /// Provider origin whose key is known to be saved in the OS key store.
    saved_origin: Option<String>,
    /// Origin of the saved key prefilled into `api_key`, until the key is
    /// edited. The key is cleared when the provider URL leaves that origin.
    prefilled_origin: Option<String>,
    microphones: Vec<String>,
    worker: Option<Worker>,
    ready: bool,
    status: String,
    error: Option<String>,
    logo: egui::TextureHandle,
    /// First launch (no settings file): show the welcome dialog until dismissed.
    welcome: bool,
}

impl Desktop {
    fn new(
        config: Result<Config>,
        path: Option<PathBuf>,
        save_path: Option<PathBuf>,
        saved_key: Result<Option<String>>,
        ctx: &egui::Context,
    ) -> Self {
        ctx.set_visuals(egui::Visuals::light());
        ctx.style_mut(|style| {
            let visuals = &mut style.visuals;
            visuals.disabled_alpha = 0.6;
            visuals.override_text_color = Some(TEXT);
            visuals.panel_fill = BACKGROUND;
            visuals.extreme_bg_color = FIELD;
            visuals.selection.bg_fill = Color32::from_rgb(219, 234, 254);
            visuals.selection.stroke = Stroke::new(1.5_f32, ACCENT);
            for widget in [
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.corner_radius = CornerRadius::same(8);
            }
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
            visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(241, 245, 249);
            visuals.widgets.hovered.bg_stroke =
                Stroke::new(1.0_f32, Color32::from_rgb(148, 163, 184));
            style.spacing.item_spacing = vec2(10.0, 8.0);
            style.spacing.button_padding = vec2(14.0, 8.0);
            style.spacing.interact_size.y = 32.0;
            for (text_style, size) in [
                (egui::TextStyle::Body, 15.0),
                (egui::TextStyle::Button, 15.0),
                (egui::TextStyle::Small, 12.5),
            ] {
                style
                    .text_styles
                    .insert(text_style, egui::FontId::proportional(size));
            }
        });
        let icon = eframe::icon_data::from_png_bytes(LOGO).expect("embedded logo is a valid PNG");
        let logo = ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..egui::TextureOptions::LINEAR
            },
        );
        // No settings file yet means nothing has been set up.
        let welcome = path.is_none() && config.is_ok();
        let loaded = match (&config, &path) {
            (Ok(_), Some(path)) => format!("Loaded {}.", path.display()),
            (Ok(_), None) => "Using built-in defaults.".to_owned(),
            (Err(_), Some(path)) => format!(
                "Could not load {}; it is left unchanged so its settings are not lost.",
                path.display()
            ),
            (Err(_), None) => "Could not load the configuration.".to_owned(),
        };
        // A file that failed to load is never overwritten with the defaults shown
        // instead: settings the window cannot edit (dictionary, recording limit)
        // and a local-only provider URL would be silently lost.
        let save_path = save_path.filter(|_| config.is_ok());
        let (config, mut error) = match config {
            Ok(config) => (config, None),
            Err(error) => (
                Config::default(),
                Some(format!(
                    "Could not load configuration: {error:#}. Defaults are shown for this session only; fix or remove the file, then restart Dictly to save settings."
                )),
            ),
        };
        let saved_key = saved_key.unwrap_or_else(|key_error| {
            error.get_or_insert_with(|| format!("{key_error:#}"));
            None
        });
        let saved_origin = saved_key
            .as_ref()
            .and_then(|_| credential::origin(config.base_url.trim()));
        Self {
            welcome,
            logo,
            source: match &save_path {
                Some(save) => format!(
                    "{loaded} Settings are saved to {} when you enable dictation.",
                    save.display()
                ),
                None => format!("{loaded} Settings apply to this session only."),
            },
            save_path,
            language: config.language.clone().unwrap_or_default(),
            authenticated: !config.api_key_env.is_empty(),
            config,
            remember_key: saved_key.is_some(),
            prefilled_origin: saved_origin.clone(),
            saved_origin,
            api_key: saved_key.unwrap_or_default(),
            microphones: microphones(),
            worker: None,
            ready: false,
            status:
                "Set up your provider, then enable dictation. No recording starts automatically."
                    .into(),
            error,
        }
    }

    /// Clears a prefilled saved key once the provider URL has another origin,
    /// so the key is never sent to a provider it was not saved for.
    fn provider_changed(&mut self) {
        if let Some(prefilled) = &self.prefilled_origin
            && credential::origin(self.config.base_url.trim()).as_ref() != Some(prefilled)
        {
            self.api_key.clear();
            self.prefilled_origin = None;
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.provider_changed();
        let mut config = self.config.clone();
        // Pasted values often carry a trailing space or newline.
        for field in [
            &mut config.base_url,
            &mut config.transcription_model,
            &mut config.cleanup_model,
        ] {
            *field = field.trim().to_owned();
        }
        config.language = if self.language.trim().is_empty() {
            None
        } else {
            Some(self.language.trim().to_owned())
        };
        if self.authenticated {
            if config.api_key_env.is_empty() {
                config.api_key_env = "OPENAI_API_KEY".into();
            }
        } else {
            config.api_key_env.clear();
        }
        if let Err(error) = config.validate() {
            self.error = Some(format!("{error:#}"));
            return;
        }
        let Some(origin) = credential::origin(&config.base_url) else {
            self.error = Some("base_url must be an HTTP or HTTPS URL".into());
            return;
        };
        // A typed key overrides the environment for this worker only.
        let key = if self.authenticated && !self.api_key.trim().is_empty() {
            Some(self.api_key.trim().to_owned())
        } else {
            None
        };
        // Saving is a convenience: failures are shown but do not block dictation.
        let mut problems = vec![];
        if let Some(path) = &self.save_path
            && let Err(error) = config.save(path)
        {
            problems.push(format!("{error:#}"));
        }
        match (&key, self.remember_key) {
            (Some(key), true) => match credential::save(&origin, key) {
                Ok(()) => self.saved_origin = Some(origin),
                Err(error) => problems.push(format!("{error:#}")),
            },
            (_, false) => {
                if let Some(saved) = self.saved_origin.take()
                    && let Err(error) = credential::delete(&saved)
                {
                    problems.push(format!("{error:#}"));
                    self.saved_origin = Some(saved);
                }
            }
            _ => {}
        }
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let (tx, events) = mpsc::channel();
        let ctx = ctx.clone();
        self.error = (!problems.is_empty()).then(|| problems.join(" "));
        self.ready = false;
        self.status = "Starting dictation…".into();
        let worker = thread::spawn(move || {
            let result = (|| {
                let pipeline = match key {
                    Some(key) => Pipeline::with_api_key(config.clone(), Some(key)),
                    None => Pipeline::new(config.clone()),
                }
                .context(
                    "Could not enable dictation. Enter an API key or check your provider settings",
                )?;
                dictation::run_controlled(config, pipeline, &signal, |message| {
                    let _ = tx.send(Event::Status(message.to_owned()));
                    ctx.request_repaint();
                })
            })();
            let _ = tx.send(Event::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
            ctx.request_repaint();
        });
        self.worker = Some(Worker {
            stop,
            events,
            thread: worker,
        });
    }

    fn poll(&mut self) {
        if let Some(worker) = &self.worker {
            // Observe completion before draining so a final error cannot be lost
            // between the last try_recv and removing the worker.
            let finished = worker.thread.is_finished();
            while let Ok(event) = worker.events.try_recv() {
                match event {
                    Event::Status(message) => {
                        self.ready = true;
                        self.status = message;
                    }
                    Event::Finished(result) => {
                        self.ready = false;
                        self.status =
                            "Dictation disabled. No new audio will be recorded or inserted.".into();
                        self.error = result.err();
                    }
                }
            }
            if finished {
                let worker = self.worker.take().unwrap();
                if worker.thread.join().is_err() {
                    self.ready = false;
                    self.error = Some(
                        "The dictation worker stopped unexpectedly. You can try enabling it again."
                            .into(),
                    );
                }
            }
        }
    }

    fn stop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.stop.store(true, Ordering::Release);
            self.status = "Stopping… Pending results will not be inserted. Requests already sent cannot be recalled.".into();
        }
    }
}

impl Drop for Desktop {
    /// Closing the window stops the worker and waits briefly so an insertion
    /// in progress can finish restoring the clipboard before the process exits.
    /// A request still waiting on the provider is abandoned after the wait.
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !worker.thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if worker.thread.is_finished() {
                let _ = worker.thread.join();
            }
        }
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested()) {
            self.stop();
        }
        self.poll();
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.welcome {
            self.welcome = !welcome(ctx, &self.logo);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BACKGROUND).inner_margin(egui::Margin::same(24)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(self.logo.id(), [56.0, 56.0]))
                                .fit_to_exact_size(vec2(56.0, 56.0)),
                        );
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.label(RichText::new("Dictly").size(30.0).strong());
                            ui.label(RichText::new("Your voice, wherever you type.").color(MUTED));
                        });
                    });
                    ui.add_space(12.0);

                    card(ui, |ui| {
                        let wide = ui.available_width() >= 480.0;
                        let mut contents = |ui: &mut egui::Ui| {
                            let (label, danger, enabled) = match &self.worker {
                                Some(worker) => ("Disable dictation", true, !worker.stop.load(Ordering::Acquire)),
                                None => ("Enable dictation", false, true),
                            };
                            if ui.add_enabled_ui(enabled, |ui| hero_button(ui, label, danger)).inner.clicked() {
                                if self.worker.is_some() {
                                    self.stop();
                                } else {
                                    self.start(ctx);
                                }
                            }
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                                ui.horizontal(|ui| {
                                    let color = if self.ready { SUCCESS } else { MUTED };
                                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 5.0, color);
                                    ui.label(RichText::new(if self.ready { "Dictation enabled" } else { "Dictation disabled" })
                                        .strong().color(color));
                                });
                                ui.label(RichText::new(&self.status).color(MUTED));
                                if let Some(error) = &self.error {
                                    ui.colored_label(DANGER, error);
                                }
                            });
                        };
                        // `horizontal` grows to its content; a left-to-right
                        // `with_layout` would centre within the whole panel height.
                        if wide {
                            ui.horizontal(&mut contents);
                        } else {
                            ui.vertical(&mut contents);
                        }
                    });
                    ui.add_space(10.0);

                    card(ui, |ui| {
                        section_title(ui, "Shortcuts");
                        ui.label(RichText::new("Enable dictation, then focus the text field where you want to write.").color(MUTED));
                        ui.add_space(4.0);
                        let keys = platform::KEYS.replace('+', " + ");
                        ui.columns(3, |columns| {
                            for (column, (key, action)) in columns.iter_mut().zip([
                                ("Space", "Hold to talk, or tap to start / finish"),
                                ("Esc", "Cancel / clear retained text"),
                                ("V", "Retry retained text insertion"),
                            ]) {
                                keycap(column, &format!("{keys} + {key}"));
                                column.label(RichText::new(action).size(13.0).color(MUTED));
                            }
                        });
                    });
                    ui.add_space(16.0);

                    ui.label(RichText::new("Settings").size(20.0).strong());
                    let locked = self.worker.is_some();
                    if locked {
                        ui.label(RichText::new("Disable dictation to change settings.").color(MUTED));
                    }
                    ui.add_enabled_ui(!locked, |ui| {
                        card(ui, |ui| {
                            section_title(ui, "Provider");
                            if field(ui, "Provider URL", egui::TextEdit::singleline(&mut self.config.base_url)).changed() {
                                self.provider_changed();
                            }
                            field(ui, "Transcription model", egui::TextEdit::singleline(&mut self.config.transcription_model));
                            toggle(ui, &mut self.authenticated, "Provider requires an API key");
                            if self.authenticated {
                                if field(ui, "API key", egui::TextEdit::singleline(&mut self.api_key)
                                    .password(true).hint_text("Paste your API key here")).changed() {
                                    self.prefilled_origin = None;
                                }
                                toggle(ui, &mut self.remember_key, &format!("Remember this key on this computer ({})", credential::STORE));
                                let env = if self.config.api_key_env.is_empty() { "OPENAI_API_KEY" } else { &self.config.api_key_env };
                                ui.label(RichText::new(if self.remember_key {
                                    "Saved for this provider and your user account when you enable dictation; never written to the settings file.".to_owned()
                                } else {
                                    format!("Used for this session only. Leave blank to use {env} from the environment.")
                                }).small().color(MUTED));
                            }
                        });
                        ui.add_space(10.0);

                        card(ui, |ui| {
                            section_title(ui, "Audio");
                            field(ui, "Language (optional)", egui::TextEdit::singleline(&mut self.language)
                                .hint_text("Auto-detect, or a two-letter code such as en or de"));
                            let label = ui.label(RichText::new("Microphone").color(MUTED));
                            ui.horizontal(|ui| {
                                let selected = match &self.config.microphone {
                                    None => "System default".to_owned(),
                                    Some(name) if self.microphones.contains(name) => name.clone(),
                                    Some(name) => format!("{name} (not connected)"),
                                };
                                egui::ComboBox::from_id_salt("microphone")
                                    .selected_text(selected)
                                    .width(ui.available_width() - 96.0)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.config.microphone, None, "System default");
                                        for name in &self.microphones {
                                            ui.selectable_value(&mut self.config.microphone, Some(name.clone()), name);
                                        }
                                    })
                                    .response
                                    .labelled_by(label.id);
                                if ui.button("Refresh").clicked() {
                                    self.microphones = microphones();
                                }
                            });
                            if self.microphones.is_empty() {
                                ui.label(RichText::new("No microphone found. Connect one, then click Refresh.").small().color(DANGER));
                            }
                            ui.add_space(4.0);
                            toggle(ui, &mut self.config.sounds, "Play sounds when recording starts and stops");
                            if cfg!(windows) {
                                toggle(ui, &mut self.config.overlay, "Show recording status on screen");
                            }
                        });
                        ui.add_space(10.0);

                        card(ui, |ui| {
                            section_title(ui, "Output");
                            toggle(ui, &mut self.config.cleanup, "Clean up transcription with AI (additional request)");
                            if self.config.cleanup {
                                field(ui, "Cleanup model", egui::TextEdit::singleline(&mut self.config.cleanup_model));
                            }
                            ui.add_space(4.0);
                            ui.label(RichText::new("Insert text by").color(MUTED));
                            segmented(ui, &mut self.config.insertion, &[(Insertion::Type, "Typing"), (Insertion::Paste, "Pasting")]);
                            ui.label(RichText::new(match self.config.insertion {
                                Insertion::Type => "Clipboard untouched; about 100 characters per second.",
                                Insertion::Paste => "Instant; your previous clipboard text is restored.",
                            }).small().color(MUTED));
                        });
                    });
                    ui.add_space(14.0);

                    ui.label(RichText::new("Audio is sent to your configured provider. AI cleanup also sends the transcript. Provider charges and privacy policies apply.").size(13.0).color(MUTED));
                    ui.label(RichText::new("Minimize this window to keep dictating. Closing it stops dictation. No tray icon or background autostart.").small().color(MUTED));
                    if let Some(origin) = &self.saved_origin {
                        ui.label(RichText::new(format!("An API key for {origin} is saved in the {}. Turn off \u{201c}Remember this key\u{201d} and enable dictation to remove it.", credential::STORE)).small().color(MUTED));
                    }
                    ui.label(RichText::new(&self.source).small().color(MUTED));
                });
            });
    }
}

fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(18))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(12),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(vec2(4.0, 18.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius::same(2), ACCENT);
        ui.label(RichText::new(title).size(17.0).strong());
    });
    ui.add_space(2.0);
}

/// A muted caption above a full-width text field, linked for screen readers.
fn field(ui: &mut egui::Ui, label: &str, edit: egui::TextEdit<'_>) -> egui::Response {
    let label = ui.label(RichText::new(label).color(MUTED));
    ui.add(edit.desired_width(f32::INFINITY).margin(vec2(10.0, 7.0)))
        .labelled_by(label.id)
}

fn keycap(ui: &mut egui::Ui, keys: &str) {
    egui::Frame::new()
        .fill(KEYCAP)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).strong());
        });
}

/// A switch reported to accessibility as a checkbox; its text also toggles it.
fn toggle(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, mut response) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
        let label = ui.add(egui::Label::new(text).wrap().sense(Sense::click()));
        if response.clicked() || label.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        response
            .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *on, text));
        if ui.is_rect_visible(rect) {
            let t = ui.ctx().animate_bool(response.id, *on);
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(11), mix(SWITCH_OFF, ACCENT, t));
            let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
            painter.circle_filled(pos2(x, rect.center().y), 8.0, Color32::WHITE);
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    })
    .inner
}

fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let last = options.len() - 1;
        for (index, &(option, text)) in options.iter().enumerate() {
            let selected = *value == option;
            let (left, right) = (
                if index == 0 { 8 } else { 0 },
                if index == last { 8 } else { 0 },
            );
            let button = egui::Button::new(RichText::new(text).color(if selected {
                Color32::WHITE
            } else {
                TEXT
            }))
            .fill(if selected { ACCENT } else { Color32::WHITE })
            .stroke(Stroke::new(1.0_f32, if selected { ACCENT } else { BORDER }))
            .corner_radius(CornerRadius {
                nw: left,
                sw: left,
                ne: right,
                se: right,
            })
            .min_size(vec2(120.0, 34.0))
            .selected(selected);
            if ui.add(button).clicked() {
                *value = option;
            }
        }
    });
}

/// The large gradient pill that enables or disables dictation.
fn hero_button(ui: &mut egui::Ui, label: &str, danger: bool) -> egui::Response {
    gradient_button(ui, label, danger, true, None, 60.0)
}

/// A gradient pill button, optionally led by a microphone badge. Without a
/// `width` it fits its label; text is centred unless the badge is shown.
fn gradient_button(
    ui: &mut egui::Ui,
    label: &str,
    danger: bool,
    microphone: bool,
    width: Option<f32>,
    height: f32,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(17.0),
        Color32::WHITE,
    );
    let fitted = if microphone {
        height + galley.size().x + 30.0
    } else {
        galley.size().x + 48.0
    };
    let (rect, response) =
        ui.allocate_exact_size(vec2(width.unwrap_or(fitted), height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    if ui.is_rect_visible(rect) {
        let (mut start, mut end) = if danger {
            (DANGER, DANGER_END)
        } else {
            (ACCENT, ACCENT_END)
        };
        let shade = if response.is_pointer_button_down_on() {
            Some((Color32::BLACK, 0.12))
        } else if response.hovered() {
            Some((Color32::WHITE, 0.12))
        } else {
            None
        };
        if let Some((toward, amount)) = shade {
            start = mix(start, toward, amount);
            end = mix(end, toward, amount);
        }
        let painter = ui.painter();
        gradient_pill(painter, rect, start, end);
        let text_left = if microphone {
            let badge = pos2(rect.left() + height / 2.0, rect.center().y);
            painter.circle_filled(badge, height / 2.0 - 6.0, Color32::from_white_alpha(46));
            microphone_glyph(painter, badge);
            rect.left() + height + 6.0
        } else {
            rect.center().x - galley.size().x / 2.0
        };
        let text_pos = pos2(text_left, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, Color32::WHITE);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The first-run dialog. Returns true once dismissed by **Get started**,
/// Escape, or a click outside it.
fn welcome(ctx: &egui::Context, logo: &egui::TextureHandle) -> bool {
    let width = (ctx.available_rect().width() - 96.0).clamp(260.0, 440.0);
    let shortcut = format!("{} + Space", platform::KEYS.replace('+', " + "));
    let modal = egui::Modal::new(egui::Id::new("welcome"))
        .backdrop_color(Color32::from_black_alpha(110))
        .frame(
            egui::Frame::new()
                .fill(Color32::WHITE)
                .corner_radius(CornerRadius::same(18))
                .inner_margin(egui::Margin::same(28))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 28,
                    spread: 0,
                    color: Color32::from_black_alpha(40),
                }),
        )
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(logo.id(), [44.0, 44.0]))
                        .fit_to_exact_size(vec2(44.0, 44.0)),
                );
                ui.label(RichText::new("Dictly").size(24.0).strong());
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Welcome to Dictly").size(26.0).strong());
            ui.label(RichText::new("Get started in three steps:").color(MUTED));
            ui.add_space(8.0);
            step(
                ui,
                1,
                "Connect a provider",
                "Check the provider URL and paste your API key under Provider. OpenAI is preset.",
                None,
            );
            step(
                ui,
                2,
                "Pick a microphone",
                "Choose an input under Audio, or keep System default.",
                None,
            );
            step(
                ui,
                3,
                "Dictate anywhere",
                "Click Enable dictation, focus any text field, then hold to talk or tap to start and stop.",
                Some(&shortcut),
            );
            ui.add_space(14.0);
            gradient_button(
                ui,
                "Get started",
                false,
                false,
                Some(ui.available_width()),
                46.0,
            )
            .clicked()
        });
    modal.inner || modal.should_close()
}

fn step(ui: &mut egui::Ui, number: u8, title: &str, body: &str, keys: Option<&str>) {
    ui.horizontal_top(|ui| {
        let (badge, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        ui.painter()
            .circle_filled(badge.center(), 15.0, Color32::from_rgb(238, 242, 255));
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            number.to_string(),
            egui::FontId::proportional(15.0),
            ACCENT,
        );
        ui.vertical(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.label(RichText::new(title).size(16.0).strong());
            ui.label(RichText::new(body).color(MUTED));
            if let Some(keys) = keys {
                keycap(ui, keys);
            }
        });
    });
    ui.add_space(6.0);
}

/// Fills a horizontal pill with a left-to-right gradient. The colour is linear
/// in x, so a triangle fan from the centre interpolates it exactly.
fn gradient_pill(painter: &egui::Painter, rect: Rect, start: Color32, end: Color32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    const STEPS: usize = 20;
    let radius = rect.height() / 2.0;
    let color_at = |x: f32| mix(start, end, (x - rect.left()) / rect.width());
    let mut outline: Vec<Pos2> = Vec::with_capacity(2 * (STEPS + 1));
    for (center, from) in [
        (pos2(rect.right() - radius, rect.center().y), -FRAC_PI_2),
        (pos2(rect.left() + radius, rect.center().y), FRAC_PI_2),
    ] {
        for step in 0..=STEPS {
            let angle = from + PI * step as f32 / STEPS as f32;
            outline.push(center + radius * vec2(angle.cos(), angle.sin()));
        }
    }
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.center(), color_at(rect.center().x));
    for &point in &outline {
        mesh.colored_vertex(point, color_at(point.x));
    }
    let count = outline.len() as u32;
    for index in 0..count {
        mesh.add_triangle(0, 1 + index, 1 + (index + 1) % count);
    }
    painter.add(egui::Shape::mesh(mesh));
    // Mesh edges are not anti-aliased; a thin matching rim smooths them.
    for (index, &a) in outline.iter().enumerate() {
        let b = outline[(index + 1) % outline.len()];
        painter.line_segment([a, b], Stroke::new(1.0_f32, color_at((a.x + b.x) / 2.0)));
    }
}

fn microphone_glyph(painter: &egui::Painter, center: Pos2) {
    use std::f32::consts::PI;
    let stroke = Stroke::new(2.0_f32, Color32::WHITE);
    painter.rect_filled(
        Rect::from_center_size(center - vec2(0.0, 4.5), vec2(10.0, 17.0)),
        CornerRadius::same(5),
        Color32::WHITE,
    );
    let arc = (0..=12)
        .map(|step| {
            let angle = PI * step as f32 / 12.0;
            pos2(
                center.x + 8.5 * angle.cos(),
                center.y - 1.0 + 8.5 * angle.sin(),
            )
        })
        .collect();
    painter.add(egui::epaint::PathShape::line(arc, stroke));
    painter.line_segment(
        [
            pos2(center.x, center.y + 7.5),
            pos2(center.x, center.y + 12.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(center.x - 5.0, center.y + 12.0),
            pos2(center.x + 5.0, center.y + 12.0),
        ],
        stroke,
    );
}

/// Linear blend of two colours; `t` is clamped to 0..=1.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
        blend(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    /// Taller than the default window so every settings field is on screen:
    /// kittest clicks at a node's position, which misses scrolled-out fields.
    const WINDOW: [f32; 2] = [620.0, 1300.0];

    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    use std::{
        sync::{LazyLock, Mutex, MutexGuard},
        time::Instant,
    };

    /// Desktop tests run one at a time: an enabled worker registers the real
    /// global shortcuts, which only one worker in the process can own.
    ///
    /// cpal caches its WASAPI device enumerator from the first thread that
    /// lists devices; once that thread exits, later device calls crash. The app
    /// lists devices on its long-lived UI thread, so tests make the first call
    /// on a thread that also stays alive.
    fn exclusive() -> MutexGuard<'static, ()> {
        static SHORTCUTS: Mutex<()> = Mutex::new(());
        static AUDIO: LazyLock<()> = LazyLock::new(|| {
            let (ready, wait) = mpsc::channel();
            thread::spawn(move || {
                microphones();
                ready.send(()).unwrap();
                loop {
                    thread::park();
                }
            });
            wait.recv().unwrap();
        });
        LazyLock::force(&AUDIO);
        SHORTCUTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn wait_for(harness: &mut Harness<'_, Desktop>, label: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.step();
            if harness.query_by_label(label).is_some() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Missing UI status: {label}; error: {:?}",
                harness.state().error
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn dismiss_welcome(harness: &mut Harness<'_, Desktop>) {
        harness.get_by_label("Get started").click();
        harness.run();
        assert!(!harness.state().welcome);
    }

    #[test]
    fn desktop_reports_setup_errors_and_can_enable_disable_and_restart() {
        let _exclusive = exclusive();
        let config = Config {
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key_env: "DICTLY_TEST_MISSING_CREDENTIAL".into(),
            ..Config::default()
        };
        let mut harness = Harness::builder()
            .with_size(WINDOW)
            .build_eframe(|cc| Desktop::new(Ok(config), None, None, Ok(None), &cc.egui_ctx));
        harness.run();
        dismiss_welcome(&mut harness);
        harness.get_by_label("Dictation disabled");
        harness.get_by_label("Enable dictation").click();
        wait_for(
            &mut harness,
            "Dictation disabled. No new audio will be recorded or inserted.",
        );
        harness
            .get_by_label_contains("Set the DICTLY_TEST_MISSING_CREDENTIAL environment variable");
        wait_for(&mut harness, "Enable dictation");

        harness
            .get_by_role_and_label(Role::TextInput, "Language (optional)")
            .click();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Language (optional)")
            .type_text("invalid");
        harness.run();
        harness.get_by_label("Enable dictation").click();
        harness.run();
        harness.get_by_label("language must be a lowercase two-letter ISO-639-1 code");
        harness
            .get_by_role_and_label(Role::TextInput, "Language (optional)")
            .click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.key_press(egui::Key::Backspace);
        harness.run();
        // A pasted provider URL with a trailing space still enables.
        harness
            .get_by_role_and_label(Role::TextInput, "Provider URL")
            .click();
        harness.run();
        harness.key_press(egui::Key::End);
        harness
            .get_by_role_and_label(Role::TextInput, "Provider URL")
            .type_text(" ");
        harness.run();
        assert!(harness.state().config.base_url.ends_with(' '));

        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .click();
        harness.run();
        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .type_text("local-test-credential");
        harness.run();
        for _ in 0..2 {
            harness.get_by_label("Enable dictation").click();
            wait_for(
                &mut harness,
                "Ready. Focus an editable text field before starting. Do not change fields until insertion finishes.",
            );
            harness.get_by_label("Dictation enabled");
            harness.get_by_label("Disable dictation").click();
            wait_for(
                &mut harness,
                "Dictation disabled. No new audio will be recorded or inserted.",
            );
            wait_for(&mut harness, "Enable dictation");
        }
    }

    #[test]
    fn enabling_after_a_failed_load_never_overwrites_the_configuration_file() {
        let _exclusive = exclusive();
        let directory =
            std::env::temp_dir().join(format!("dictly-unloadable-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        let original =
            "base_url = \"http://127.0.0.1:9/v1\"\ndictionary = [\"Kubernetes\"]\nclenup = true\n";
        std::fs::write(&path, original).unwrap();
        let loaded = Config::load(Some(&path));
        assert!(loaded.is_err());
        let (shown, saved) = (path.clone(), path.clone());
        let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
            Desktop::new(loaded, Some(shown), Some(saved), Ok(None), &cc.egui_ctx)
        });
        harness.run();
        harness.get_by_label_contains("it is left unchanged");
        assert!(
            harness.query_by_label_contains("Loaded ").is_none(),
            "a file that failed to load must not be reported as loaded"
        );
        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .click();
        harness.run();
        harness
            .get_by_role_and_label(Role::PasswordInput, "API key")
            .type_text("local-test-credential");
        harness.run();
        harness.get_by_label("Enable dictation").click();
        wait_for(
            &mut harness,
            "Ready. Focus an editable text field before starting. Do not change fields until insertion finishes.",
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        harness.get_by_label("Disable dictation").click();
        wait_for(&mut harness, "Enable dictation");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_saved_key_is_never_offered_to_another_provider_origin() {
        let _exclusive = exclusive();
        let config = Config {
            base_url: "http://127.0.0.1:9/v1".into(),
            ..Config::default()
        };
        let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
            Desktop::new(
                Ok(config),
                None,
                None,
                Ok(Some("saved-key".into())),
                &cc.egui_ctx,
            )
        });
        harness.run();
        dismiss_welcome(&mut harness);
        assert_eq!(harness.state().api_key, "saved-key");
        harness.get_by_label_contains("An API key for http://127.0.0.1:9 is saved");
        // Another path on the same origin keeps the saved key.
        harness
            .get_by_role_and_label(Role::TextInput, "Provider URL")
            .click();
        harness.run();
        harness.key_press(egui::Key::End);
        harness
            .get_by_role_and_label(Role::TextInput, "Provider URL")
            .type_text("/");
        harness.run();
        assert_eq!(harness.state().api_key, "saved-key");
        // Another host drops it before it can be sent anywhere.
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, "Provider URL")
            .type_text("https://attacker.example/v1");
        harness.run();
        assert_eq!(
            harness.state().config.base_url,
            "https://attacker.example/v1"
        );
        assert!(harness.state().api_key.is_empty());
    }

    #[test]
    fn closing_waits_for_the_worker_to_finish_restoring_the_clipboard() {
        let _exclusive = exclusive();
        let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
            Desktop::new(Ok(Config::default()), None, None, Ok(None), &cc.egui_ctx)
        });
        harness.run();
        // Stands in for a paste that keeps running for its 400 ms settle after
        // the stop request, then restores the clipboard.
        let stop = Arc::new(AtomicBool::new(false));
        let restored = Arc::new(AtomicBool::new(false));
        let (signal, done) = (Arc::clone(&stop), Arc::clone(&restored));
        let (_tx, events) = mpsc::channel();
        harness.state_mut().worker = Some(Worker {
            stop,
            events,
            thread: thread::spawn(move || {
                while !signal.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                thread::sleep(Duration::from_millis(400));
                done.store(true, Ordering::Release);
            }),
        });
        drop(harness);
        assert!(restored.load(Ordering::Acquire));
    }
}
