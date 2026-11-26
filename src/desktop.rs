use crate::{
    capture::microphones,
    config::{Config, Insertion},
    dictation,
    pipeline::Pipeline,
    platform::{self, credential},
};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};
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
            .with_min_inner_size([440.0, 540.0]),
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
            style.visuals.disabled_alpha = 0.75;
            style.spacing.item_spacing = egui::vec2(10.0, 9.0);
            style.spacing.button_padding = egui::vec2(14.0, 9.0);
            style.spacing.interact_size.y = 30.0;
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        });
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(248, 250, 253)).inner_margin(24.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(RichText::new("Dictly").size(32.0).strong());
                    ui.label("Your voice, wherever you type.");
                    ui.add_space(8.0);
                    egui::Frame::group(ui.style()).fill(Color32::WHITE).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(if self.ready { "Dictation enabled" } else { "Dictation disabled" });
                            if self.worker.is_some() {
                                let stopping = self.worker.as_ref().unwrap().stop.load(Ordering::Acquire);
                                if ui.add_enabled(!stopping, egui::Button::new(RichText::new("Disable dictation").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(165, 32, 42))).clicked() {
                                    self.stop();
                                }
                            } else if ui.add(egui::Button::new(RichText::new("Enable dictation").color(Color32::WHITE))
                                .fill(Color32::from_rgb(32, 103, 220))).clicked() {
                                self.start(ctx);
                            }
                        });
                        ui.label(&self.status);
                        if let Some(error) = &self.error {
                            ui.colored_label(Color32::from_rgb(165, 32, 42), error);
                        }
                    });
                    ui.add_space(6.0);
                    ui.add_enabled_ui(self.worker.is_none(), |ui| {
                        let label = ui.label("Provider URL");
                        if ui.add(egui::TextEdit::singleline(&mut self.config.base_url).desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
                            self.provider_changed();
                        }
                        let label = ui.label("Transcription model");
                        ui.add(egui::TextEdit::singleline(&mut self.config.transcription_model).desired_width(f32::INFINITY)).labelled_by(label.id);
                        ui.checkbox(&mut self.authenticated, "Provider requires an API key");
                        if self.authenticated {
                            let label = ui.label("API key");
                            if ui.add(egui::TextEdit::singleline(&mut self.api_key).password(true)
                                .hint_text("Paste your API key here").desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
                                self.prefilled_origin = None;
                            }
                            ui.checkbox(&mut self.remember_key, format!("Remember this key on this computer ({})", credential::STORE));
                            let env = if self.config.api_key_env.is_empty() { "OPENAI_API_KEY" } else { &self.config.api_key_env };
                            ui.small(if self.remember_key {
                                "Saved for this provider and your user account when you enable dictation; never written to the settings file.".to_owned()
                            } else {
                                format!("Used for this session only. Leave blank to use {env} from the environment.")
                            });
                        }
                        let label = ui.label("Language (optional)");
                        ui.add(egui::TextEdit::singleline(&mut self.language)
                            .hint_text("Auto-detect, or a two-letter code such as en or de").desired_width(f32::INFINITY)).labelled_by(label.id);
                        ui.checkbox(&mut self.config.cleanup, "Clean up transcription with AI (additional request)");
                        if self.config.cleanup {
                            let label = ui.label("Cleanup model");
                            ui.add(egui::TextEdit::singleline(&mut self.config.cleanup_model).desired_width(f32::INFINITY)).labelled_by(label.id);
                        }
                        let label = ui.label("Microphone");
                        ui.horizontal(|ui| {
                            let selected = match &self.config.microphone {
                                None => "System default".to_owned(),
                                Some(name) if self.microphones.contains(name) => name.clone(),
                                Some(name) => format!("{name} (not connected)"),
                            };
                            egui::ComboBox::from_id_salt("microphone")
                                .selected_text(selected)
                                .width(ui.available_width() - 90.0)
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
                            ui.small("No microphone found. Connect one, then click Refresh.");
                        }
                        ui.checkbox(&mut self.config.sounds, "Play sounds when recording starts and stops");
                        if cfg!(windows) {
                            ui.checkbox(&mut self.config.overlay, "Show recording status on screen");
                        }
                        ui.label("Insert text by");
                        ui.radio_value(&mut self.config.insertion, Insertion::Type, "Typing: clipboard untouched; about 100 characters per second");
                        ui.radio_value(&mut self.config.insertion, Insertion::Paste, "Pasting: instant; previous clipboard text is restored");
                    });
                    ui.add_space(6.0);
                    egui::Frame::group(ui.style()).fill(Color32::WHITE).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.strong("Dictate in another app");
                        ui.label("Enable dictation, then focus the text field where you want to write.");
                        egui::Grid::new("shortcuts").spacing([24.0, 8.0]).show(ui, |ui| {
                            let keys = platform::KEYS.replace('+', " + ");
                            for (key, action) in [
                                ("Space", "Hold to talk, or tap to start / finish"),
                                ("Esc", "Cancel / clear retained text"),
                                ("V", "Retry retained text insertion"),
                            ] {
                                ui.label(RichText::new(format!("{keys} + {key}")).strong());
                                ui.label(action);
                                ui.end_row();
                            }
                        });
                    });
                    ui.label("Audio is sent to your configured provider. AI cleanup also sends the transcript. Provider charges and privacy policies apply.");
                    ui.small("Minimize this window to keep dictating. Closing it stops dictation. No tray icon or background autostart.");
                    if let Some(origin) = &self.saved_origin {
                        ui.small(format!("An API key for {origin} is saved in the {}. Uncheck \u{201c}Remember this key\u{201d} and enable dictation to remove it.", credential::STORE));
                    }
                    ui.small(&self.source);
                });
            });
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn desktop_reports_setup_errors_and_can_enable_disable_and_restart() {
        let _exclusive = exclusive();
        let config = Config {
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key_env: "DICTLY_TEST_MISSING_CREDENTIAL".into(),
            ..Config::default()
        };
        let mut harness = Harness::builder()
            .with_size([620.0, 820.0])
            .build_eframe(|cc| Desktop::new(Ok(config), None, None, Ok(None), &cc.egui_ctx));
        harness.run();
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
        let mut harness = Harness::builder()
            .with_size([620.0, 820.0])
            .build_eframe(|cc| {
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
        let mut harness = Harness::builder()
            .with_size([620.0, 820.0])
            .build_eframe(|cc| {
                Desktop::new(
                    Ok(config),
                    None,
                    None,
                    Ok(Some("saved-key".into())),
                    &cc.egui_ctx,
                )
            });
        harness.run();
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
        let mut harness = Harness::builder()
            .with_size([620.0, 820.0])
            .build_eframe(|cc| {
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
