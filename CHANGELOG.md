# Changelog

User-visible changes to Dictly. Versions match `Cargo.toml`. Local build entries do not imply a published release, signed installer, or completed Windows desktop verification.

## Unreleased

### Added

- New Dictly logo (a microphone with a sound wave and text cursor), used in the README, as the desktop window icon, and as the Windows executable icon.
- Redesigned desktop window: logo header, a large gradient **Enable dictation** button beside the status, shortcut key caps, and settings grouped into Provider, Audio, and Output cards with toggle switches and a **Typing | Pasting** switch. A note now explains why settings are locked while dictation is enabled. Layout follows the `docs/design/settings-light.png` mockup.
- Redesigned status overlay: a dark pill with a microphone badge and a state icon. Recording shows a pulsing badge and a running clock, transcription a spinner, then a green check (inserted), amber warning (text retained), red outlined pill (error), or gray cross (canceled). It is drawn supersampled for smooth edges and is wider (400 px at 100% scale) so shortcut hints fit beside the icon.
- Live dictation, the desktop window, and saved API keys on macOS and Linux (X11). macOS uses Control+Option shortcuts and the Keychain, and needs Accessibility access; Linux uses Ctrl+Alt and a Secret Service keyring. The status overlay stays Windows-only. Wayland is not supported.
- Vercel AI Gateway as a provider: with `base_url = "https://ai-gateway.vercel.sh/v1"`, transcription uses the gateway's base64 JSON endpoint and cleanup its chat completions.
- Continuous integration on GitHub Actions: `cargo fmt --check`, strict Clippy with and without `test-audio`, debug and release tests on Windows, and `cargo deny` / `cargo audit` with a committed `deny.toml`. Unsafe blocks without a `// SAFETY:` comment now fail the build.
- Hold-to-talk: holding Ctrl+Alt+Space for at least half a second records until release. A quick tap still toggles recording.
- Start, stop, cancel, and error sounds, plus an always-on-top status overlay that does not take focus or mouse input. Both can be turned off, and the overlay scales with display DPI.
- Microphone picker in the window, a `microphone` setting, and a `dictly devices` command that lists input names.
- Settings are saved to `config.toml` when dictation is enabled from the window. An optional checkbox stores the API key in Windows Credential Manager instead of the file.
- Optional paste insertion (`insertion = "paste"`). It marks dictated clipboard text to be skipped by clipboard history and cloud sync, and restores the previous clipboard text. It types instead when the clipboard holds anything other than plain text.
- One automatic transcription retry after HTTP 429, 500, 502, 503, or 504, or a failed connection. It honors `Retry-After` up to 5 seconds.
- Single-instance guard with a clear "already running" message.
- Windows end-to-end harness (`scripts/windows_e2e.py`) and a `test-audio` Cargo feature that replays a WAV file in place of the microphone, for testing only.
- Windows desktop window opened by double-clicking `dictly.exe`, with provider/model/language settings, optional cleanup, enable/disable controls, and visible status/errors.
- Masked session-only API-key entry without storing keys on disk or changing the process environment.
- Automatic loading of `config.toml` beside the executable for desktop launch; explicit `--config` overrides it. CLI configuration loading is unchanged.
- Desktop interaction tests for setup errors and service restart, plus session-credential and CLI compatibility contract tests.
- Usage guide covering setup, shortcuts, cancellation, recovery, privacy, and troubleshooting.
- Complete configuration reference with built-in defaults, local-provider examples, and required API contracts.
- Development guide with native/cross-build instructions, automated checks, and a pending Windows verification checklist.

### Changed

- Renamed the app from SpeakCraft to Dictly: the executable is now `dictly.exe`, the crate is `dictly`, and test variables use the `DICTLY_` prefix. An API key saved in Windows Credential Manager under the old name must be saved again.
- SpeakCraft is now licensed under GPL-3.0-or-later (previously MIT).
- The default transcription model is now `gpt-4o-mini-transcribe` (previously `whisper-1`). In a live comparison it resolved a spoken self-correction that `whisper-1` turned into an invented date after cleanup. Set `transcription_model = "whisper-1"` to keep the old model.
- Recordings above 16 kHz are downsampled to 16 kHz before upload, which cuts the upload to a third for 48 kHz microphones.
- AI cleanup receives the dictionary terms so their spelling survives editing.
- Canceling, disabling, or closing during processing now also skips any pending retry and the optional cleanup request.
- Without a Windows default input device, recording uses the first available microphone instead of failing.
- Launching without a subcommand opens the desktop UI instead of printing help and exiting. CLI `run`, `transcribe`, `check`, help, and version remain available.
- Minimize keeps dictation enabled; disabling or closing releases shortcuts and discards pending insertion and retained text. Dictly's own settings window is excluded from dictation targets.
- Renamed the app to SpeakCraft; the Rust package and executable are now `speakcraft` and `speakcraft.exe` on Windows. Existing configuration files, environment-variable names, and dictation shortcuts remain compatible. Update launch commands to the new executable path.
- Reorganized the README around Windows quick start, features, build instructions, and links to detailed documentation.
- Documented the difference between the original archive and the local Dictly build, upgrade steps, file-transcription usage, and unchanged configuration behavior.
- Made the MVP status, API costs, cloud data flow, and unverified Windows desktop behavior explicit.
- Capture now requests 16 kHz mono when the microphone offers it, falling back to the device default sample rate otherwise. Fallback audio is downmixed to mono without local resampling; provider sample-rate support determines compatibility. Echo cancellation, noise suppression, and auto gain remain at the Windows defaults.

### Fixed

- Typed insertion into Windows 11 Notepad arrived as one character repeated, such as `plain ssss…`, because Notepad reads the most recent injected character when it processes each keystroke. Dictly now types one character at a time, about 100 per second, and stops cleanly if focus changes or Esc is pressed mid-insertion.
- Updated `rustls-webpki` to 0.103.17 and `webpki-roots` to 1.0.12 for TLS certificate-verification advisories RUSTSEC-2026-0382 and RUSTSEC-2026-0381, and replaced the yanked `clipboard-win` 5.4.1 with 5.4.3.
- Updated `url` to 2.5.9 for RUSTSEC-2026-0592, `rustix` to 1.1.6 for the unsound advisory RUSTSEC-2026-0486, and replaced the yanked `async-executor` 1.14.0 with 1.14.1.
- The desktop window no longer overwrites a configuration file it failed to load. Previously one typo made **Enable dictation** replace the file with defaults, losing the dictionary and recording limit and switching a local provider URL to OpenAI. The defaults now apply to that session only, and the window says the file was left unchanged.
- Plain-HTTP loopback providers are no longer reached through `HTTP_PROXY` or `ALL_PROXY` from the environment. Previously a configured proxy received the audio, the API key, and the transcript in cleartext, and could return any text to be typed. HTTPS providers still honor proxy variables.
- An API key emptied in the Windows Credential Manager control panel no longer aborts debug builds at launch or reads a null pointer in release builds. It is reported as unreadable so a new key can be entered.
- The documented retry after a failed connection now happens for real recordings. Uploads above about 6 KB reported a refused connection as a body error, so it was never retried; the upload is now sent as one in-memory body.
- A provider URL pasted with a trailing space passed validation but sent every request to a wrong path such as `/v1%20/audio/transcriptions`. The window now trims the provider URL and model names, and `check` rejects a `base_url` with surrounding spaces.
- Transcription and cleanup replies larger than 1 MB are now rejected before parsing. Previously the whole reply was held in memory, so a broken or malicious provider could exhaust memory: a 400 MB reply took 811 MB.
- Inserted text no longer carries bidirectional controls (U+202A–U+202E, U+2066–U+2069) or invisible zero-width characters (U+200B, U+2060, U+FEFF), which a provider could use to reorder or hide text; the zero-width joiner in emoji and the zero-width non-joiner used in Persian and Indic spelling stay. CR LF and lone CR line breaks are inserted as LF. Whether a line break acts as Enter in terminals and chat apps is still unverified.
- `transcribe` no longer passes escape sequences from the provider to an interactive terminal, where they could retitle or clear the window, recolour text, or write the clipboard. Control characters other than line feed and tab are shown as U+FFFD when stdout is a terminal; redirected output is unchanged.
- A saved API key is now bound to its provider origin (stored as `Dictly/API key/<origin>`, such as `Dictly/API key/https://api.openai.com`). Previously one fixed entry was filled in for any provider, so a copied `config.toml` pointing at another server received the key with the first dictation. The window now fills in a saved key only when the loaded provider URL has the same origin, clears it when the URL is changed to another origin, and saves under the current origin. A key saved by an earlier build is no longer filled in; enter it once more to save it for its provider, which also removes the old unbound entry.
- Microphones running at high sample rates no longer stop recording early. The size cap was applied before downsampling, so a 192 kHz microphone stopped after about 62 seconds although its 16 kHz upload was about 2 MB. Recording now runs for the configured duration, the 24 MB cap applies to the downsampled upload, and the audio buffer grows as needed instead of reserving up to 24 MB for every recording.
- Typing interrupted by Esc or a focus change no longer leaves half of an emoji or other character outside the Basic Multilingual Plane in the target. Both UTF-16 halves are now sent together, interruptions are checked between characters, and "Insertion stopped after N of M characters" counts characters instead of UTF-16 units.
- Paste mode keeps the clipboard's locale (`CF_LOCALE`) when it restores the previous text, so apps reading the ANSI text get the same conversion as before. It also no longer leaks a small allocation when Windows cannot register a clipboard-privacy format.
- Closing the window, or pressing Ctrl+C in console `run`, during a paste no longer exits with the dictated text left on the clipboard. Dictly now stops dictation and waits up to one second for the paste to finish and restore the previous clipboard text before exiting.
- Cleanup warnings now name the cause, such as `HTTP 401 Unauthorized`, `Cleanup returned empty text`, or an oversized reply, instead of only saying that cleanup failed.
- A `config.toml` saved as UTF-16 (PowerShell 5's `>` default) is reported as UTF-16 instead of as a confusing TOML parse error. UTF-8 files with a byte-order mark load.
- Saving settings writes a temporary file and renames it over `config.toml`, so an interrupted save no longer leaves a truncated file.
- CLI API keys read from the environment are trimmed like keys typed in the window. A trailing space or line break previously produced a confusing authentication failure.

On 2026-10-16, 12 contract tests and 2 Windows unit tests passed, along with strict Clippy with and without `test-audio`. The end-to-end harness passed 13 scenarios against the release executable: hold-to-talk, tap-to-toggle, both cancel paths, a provider error, focus change with recovery, held modifiers, second instance, Notepad typing, Notepad pasting, clipboard restore, the recording limit, and one live OpenAI dictation with cleanup. Recorded TTS audio replaced the microphone because the test machine had no connected input device, so live microphone capture remains unverified.

On 2026-10-15, the desktop build passed eight contract tests and a Windows desktop interaction test. Native default, enabled/cleanup, and missing-credential windows were rendered and inspected. Live microphone capture, hotkey-driven recording, text insertion into other apps, and speech-model accuracy remain unverified. No desktop archive, installer, or public release was published.

## 0.1.0 — 2026-10-14

Initial local MVP. Source and an unsigned Windows x64 ZIP were prepared; no release was published.

### Added

- Windows global toggle, cancellation, and retained-transcript recovery shortcuts.
- Default-microphone capture using CPAL/WASAPI, mono PCM WAV encoding in memory, duration/size limits, and rejection of short or effectively silent recordings.
- OpenAI-compatible transcription with optional language and dictionary hints.
- Opt-in chat-completions cleanup with original-transcript fallback on errors or empty output.
- Unicode text insertion without clipboard replacement, with best-effort focus checks and retained text for explicit retry.
- `run`, `transcribe`, and `check` commands; file transcription and configuration checks are available outside Windows.
- TOML configuration, environment-variable credentials, HTTPS enforcement except loopback, redirect rejection, and request timeouts.
- Six contract tests, Linux orb setup, locked dependencies, Windows GNU cross-build support, and MIT license.

### Verification

- Six automated tests passed on Linux, and an HTTP-fixture CLI smoke check verified Unicode stdout.
- Formatting and strict Clippy checks passed, including Windows-target code.
- A Windows x64 release executable cross-built successfully and was packaged with checksum information.
- No real Windows desktop test or live speech-model accuracy test was performed.

### Known limitations

- Console-only app with fixed shortcuts and default microphone; no tray UI, installer, push-to-talk, or bundled offline model.
- A configured speech service is required. Optional cleanup uses a separate request to the same provider.
- Canceling during processing suppresses insertion but does not retract requests or stop optional cleanup already scheduled on the worker.
- Focus protection is best-effort. Elevated or incompatible target apps can block or ignore input; successful event injection does not prove text was inserted.

See [usage and limitations](docs/usage.md) for the full operating constraints.
