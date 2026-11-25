use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

/// How Windows dictation inserts text into the focused field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Insertion {
    /// Type characters one at a time; the clipboard is never touched.
    #[default]
    Type,
    /// Paste through the clipboard, then restore its previous text.
    Paste,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub base_url: String,
    pub api_key_env: String,
    pub transcription_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub dictionary: Vec<String>,
    pub cleanup: bool,
    pub cleanup_model: String,
    pub max_recording_seconds: u32,
    /// Input device name; omitted uses the Windows default, then the first available input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// Short tones when recording starts, stops, or fails.
    pub sounds: bool,
    /// Always-on-top status pill that never takes keyboard focus.
    pub overlay: bool,
    pub insertion: Insertion,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            transcription_model: "gpt-4o-mini-transcribe".into(),
            language: None,
            dictionary: vec![],
            cleanup: false,
            cleanup_model: "gpt-4o-mini".into(),
            max_recording_seconds: 120,
            microphone: None,
            sounds: true,
            overlay: true,
            insertion: Insertion::Type,
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config: Self = match path {
            Some(path) => toml::from_str(&read_text(path)?).context("Invalid configuration")?,
            None => Self::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Write non-secret settings. Comments in an existing file are not preserved.
    /// The file is replaced atomically, so a crash or full disk never leaves it truncated.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = format!(
            "# Saved by Dictly. No API key belongs in this file.
{}",
            toml::to_string(self).context("Cannot serialize configuration")?
        );
        let name = path.file_name().context("Settings path has no file name")?;
        let temporary = path.with_file_name(format!(".{}.tmp", name.to_string_lossy()));
        let written = std::fs::File::create(&temporary)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temporary, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written.with_context(|| format!("Cannot save settings to {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        // The URL parser ignores surrounding spaces, but requests would keep
        // them inside the path (for example /v1%20/audio/transcriptions).
        ensure!(
            self.base_url.trim() == self.base_url,
            "base_url must not start or end with spaces"
        );
        let url = reqwest::Url::parse(&self.base_url).context("Invalid base_url")?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && loopback),
            "base_url must use HTTPS (HTTP is allowed only on loopback)"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "base_url must not contain credentials, query parameters, or a fragment"
        );
        ensure!(
            (1..=120).contains(&self.max_recording_seconds),
            "max_recording_seconds must be between 1 and 120"
        );
        ensure!(
            !self.transcription_model.trim().is_empty()
                && (!self.cleanup || !self.cleanup_model.trim().is_empty()),
            "Model names must not be empty"
        );
        ensure!(
            self.microphone
                .as_ref()
                .is_none_or(|name| !name.trim().is_empty()),
            "microphone must be omitted or a nonblank device name"
        );
        if let Some(language) = &self.language {
            ensure!(
                language.len() == 2 && language.bytes().all(|b| b.is_ascii_lowercase()),
                "language must be a lowercase two-letter ISO-639-1 code"
            );
        }
        Ok(())
    }
}

/// Read a configuration file as UTF-8, naming the encoding problem when it is UTF-16.
fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).context("Cannot read configuration")?;
    // PowerShell 5's `>` and `Out-File` write UTF-16 by default.
    ensure!(
        !bytes.starts_with(&[0xFF, 0xFE]) && !bytes.starts_with(&[0xFE, 0xFF]),
        "Configuration file {} is saved as UTF-16; save it as UTF-8 instead",
        path.display()
    );
    let text = String::from_utf8(bytes).context("Configuration file is not valid UTF-8")?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(text) => text.to_owned(),
        None => text,
    })
}
