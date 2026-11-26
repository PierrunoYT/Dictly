# Usage, privacy, and troubleshooting

[README](../README.md) · [Configuration](configuration.md) · [Development](development.md) · [Changelog](../CHANGELOG.md)

## Install and start

The desktop app targets Windows 10/11 x64. [Build from source](development.md#windows-build) to obtain the current `dictly.exe`, then double-click it. No terminal is needed. If you have a supplied ZIP containing the desktop build, extract it first; do not run the executable inside the ZIP. The app is unsigned and has no installer, auto-updater, or background service. No new desktop archive or public release has been published.

For an archive that includes `dictly.exe` and `SHA256SUMS.txt`, compare the executable's SHA-256 digest with its checksum entry:

```powershell
Get-FileHash .\dictly.exe -Algorithm SHA256
Get-Content .\SHA256SUMS.txt
```

A matching checksum detects corruption, not publisher authenticity: both files came from the same archive. Only run code from a source you trust. Alternatively, [build from source](development.md#windows-build).

Older archives use `rust-dictate.exe`; use that filename for their commands and checksum checks. A locally rebuilt `dictly.exe` will not match an older binary's checksum. When upgrading, close the old instance, keep your existing `config.toml`, and update any launch commands or shortcuts to the new executable path. The configuration keys, `OPENAI_API_KEY` variable, and dictation shortcuts are unchanged; no configuration migration is required.

Follow the [README quick start](../README.md#quick-start-windows-desktop) to configure the provider and enter your API key in the window. Click **Enable dictation** and wait for **Ready**. Missing credentials, invalid settings, or unavailable shortcuts appear in the window instead of closing it. Enabling dictation alone makes no provider request.

Desktop launch reads an explicit `--config` path, otherwise `config.toml` beside the executable, otherwise built-in defaults. A file-load error is shown in the window with default settings for review. Enabling dictation saves the window's settings to that same path (or beside the executable), without comments; a failed save is reported but does not block dictation. Entered keys are session-only unless **Remember this key on this computer** is checked, which stores the key in Windows Credential Manager, the macOS Keychain, or a Linux Secret Service keyring (GNOME Keyring, KWallet) for your account, bound to the provider origin (scheme, host, and port) of the **Provider URL**. A saved key is filled in only when the loaded provider URL has that origin, so a copied `config.toml` that points elsewhere never receives it; changing the provider URL to another origin clears it. Uncheck **Remember this key on this computer** and enable dictation again to remove the stored key. Disable dictation before editing settings. Keep secrets out of configuration files.

On macOS and Linux, see [macOS and Linux](#macos-and-linux) for permissions and differences. In Windows settings:

- **Windows 11:** Privacy & security → Microphone → allow microphone access and desktop app access.
- **Windows 10:** Privacy → Microphone → allow microphone access and desktop app access.
- **Both:** choose a microphone in Dictly's **Microphone** list, or keep **System default** and select the input in Sound settings. If Windows reports no default input, Dictly uses the first available microphone. Click **Refresh** after connecting a device.

Minimize the window to keep dictation enabled. Closing it exits the app, stops capture, and discards pending insertion and retained text; if a paste is in progress, Dictly waits up to one second for it to restore your previous clipboard text before exiting. Ctrl+C or closing the console stops `dictly run` the same way. **Disable dictation** does the same without closing the window and releases the global shortcuts. An API request already sent may finish in the background, but its result is not inserted. Only one Dictly (window or `run`) can be open at a time. No tray icon is installed.

## macOS and Linux

Live dictation, the desktop window, and saved keys also work on macOS and on Linux with an X11 session. [Build from source](development.md#macos-and-linux-builds) and run `target/release/dictly`. Differences from Windows:

| | macOS | Linux (X11) |
| --- | --- | --- |
| Shortcuts | **Control+Option**+Space, Esc, V | **Ctrl+Alt**+Space, Esc, V |
| Permissions | Allow the app that starts Dictly (Terminal, or Dictly) in System Settings › Privacy & Security › **Accessibility** and **Microphone**. Dictly shows the system prompt when Accessibility is missing. | None. Wayland sessions are not supported: Wayland does not let apps register global shortcuts or type into other windows. Log in with an X11 (Xorg) session. |
| Saved API key | macOS Keychain | Secret Service keyring; without one, keys are session-only and saving reports an error |
| Paste shortcut | Command+V | Ctrl+V |
| Status overlay | Not shown; sounds and the window's status line report progress | Not shown |

Everything else (hold-to-talk, tap-to-toggle, cancel, recovery, focus checks, typing and paste insertion) behaves as described below. Replace Ctrl+Alt with Control+Option on macOS.

## Dictate into an application

1. Enable dictation in Dictly, then open a normal, non-administrator Notepad window and focus its editing area.
2. Hold **Ctrl+Alt+Space**. A rising tone plays and a red **Recording** pill appears above the taskbar.
3. Speak while holding the shortcut, then release it. A falling tone plays and the pill shows **Transcribing…**.
4. Wait for transcription and insertion. Do not move the caret, switch fields, or type while processing.
5. Review the inserted text, especially names, numbers, and any AI corrections.

For longer dictation, tap the shortcut instead: a press shorter than half a second keeps recording hands-free until you press it again. Turn off sounds or the status pill in the window, or with `sounds` and `overlay` in `config.toml`. The pill never takes focus or mouse clicks. Shortcuts are fixed in this version. Ctrl+Alt combinations may overlap with AltGr on some keyboard layouts.

Recording stops and proceeds to transcription automatically at the configured duration (120 seconds by default). Audio above 16 kHz is downsampled to 16 kHz before upload, so the longest recording is under 4 MB, well below the 24 MB upload cap. Recordings under 200 ms and effectively silent input are rejected. A microphone error discards the recording.

### Cancel or recover text

| State | Ctrl+Alt+Esc | Ctrl+Alt+V |
| --- | --- | --- |
| Recording | Discard audio without submitting it; clear retained text | No action |
| Processing | Suppress the result; clear retained text; wait for the request to finish | No action |
| Idle | Clear retained text | Insert retained text into the currently focused field |

Canceling during processing does **not** abort or retract the API request already in flight, but it prevents any retry and the optional cleanup request. Another recording cannot start until that request finishes. Closing the app cannot retract a request already received by the provider either.

If a focus change is detected or insertion fails, Dictly reports that text was retained. Focus the intended field, press **Ctrl+Alt+V**, and release the keys. If partial text appeared earlier, remove it before retrying to avoid duplication. Only one transcript can be retained; it is not a history feature. Use cancellation to clear it explicitly. Dictly does not record for or insert text into its own settings window.

## Command reference

| Command | Behavior |
| --- | --- |
| `dictly` | Open the desktop window; on Windows also works by double-clicking the executable |
| `dictly --config config.toml` | Open the desktop window using an explicit configuration file |
| `dictly --config config.toml run` | Console-mode live dictation; reads environment credentials and reserves shortcuts at startup |
| `dictly --config config.toml transcribe recording.wav` | Upload an existing file and print the transcript to stdout |
| `dictly --config config.toml check` | Validate configuration without contacting a provider or opening the microphone |
| `dictly devices` | List microphone names for the `microphone` setting |
| `dictly --help` | Show commands and options |
| `dictly --version` | Show the package version |

Use `.\dictly.exe` as the command in Windows PowerShell, or `.\target\release\dictly.exe` when running from the source root. Explicit relative paths resolve against the current working directory. `--config` can appear before or after a subcommand. For `run`, `transcribe`, and `check`, omitting it uses built-in defaults; only desktop launch automatically checks beside the executable.

CLI commands still use environment credentials. To enter a key without putting it in shell history, use the same PowerShell session to set it and launch the command:

```powershell
$secret = Read-Host "OpenAI API key" -AsSecureString
$env:OPENAI_API_KEY = [System.Net.NetworkCredential]::new("", $secret).Password
.\dictly.exe --config config.toml run
```

This does not persist the key to future sessions. A desktop app launched separately from Explorer does not inherit a variable set only in this PowerShell session; use its API-key field instead.

`transcribe` accepts nonempty files up to 24,000,000 bytes. Formats depend on the provider; the app does not convert existing files. It uses the same optional cleanup setting as live dictation, but never inserts text into another application. Warnings go to stderr; the transcript goes to stdout. Redirect stdout only if you intend to save that text. When stdout is a terminal, control characters other than line feed and tab (such as escape sequences and carriage returns from the provider) are shown as `�` so they cannot retitle, clear, or recolour the terminal. Redirected output keeps the provider's text unchanged; do not pipe it into a shell.

For example, transcribe an existing file after configuring provider credentials:

```powershell
.\dictly.exe --config config.toml transcribe .\recording.wav
```

This command uploads the file to the configured provider and may incur charges; it is not an offline validation command.

`check` does not verify the key, provider, microphone, shortcuts, or application compatibility. Startup/command failures exit nonzero. Live dictation logs individual recording or provider errors and remains available for another attempt.

## Privacy and limitations

- **Network:** audio goes to the configured speech endpoint after recording stops, including automatic stops. When cleanup is enabled, the transcript goes to the same base URL's chat-completions endpoint. With Vercel AI Gateway, the gateway forwards both to the model's provider, so both Vercel's and that provider's policies apply. Provider charges and retention policies apply; this app does not control them.
- **Local data:** live recordings and retained transcripts stay in process memory. The app writes no recording files, transcript history, analytics, or screen captures. Enabling dictation from the window writes non-secret settings to `config.toml`. Dictation does not read application content. With the default typing insertion it never touches the clipboard; see **Insertion** for paste mode. Ordinary memory deallocation is not secure erasure; Windows paging and crash dumps are outside its control.
- **Credentials:** the desktop API-key field is masked and kept in process memory. It is stored only if you check **Remember this key on this computer**, and then only in the OS key store for your account (Windows Credential Manager, macOS Keychain, or a Linux Secret Service keyring), never in `config.toml`. On Windows and with an unlocked Linux keyring, any process running as your account can read that entry. A typed key overrides the configured environment variable; leaving the field blank uses that variable. Unchecking **Provider requires an API key** omits authentication. CLI commands read environment credentials. Do not store keys in TOML, paste them into issue reports, or share environment dumps. Network requests require HTTPS except for explicit loopback addresses, and redirects are rejected. Plain-HTTP loopback requests always connect directly and ignore `HTTP_PROXY`/`ALL_PROXY`; HTTPS providers honor `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY`, and TLS stays end to end through the proxy.
- **Focus:** protection is best-effort, not a security boundary. Checks use foreground-window and focused-control handles. Browser fields can share a handle; moving the caret and very brief focus changes may go undetected. Do not dictate into password fields or terminals.
- **Insertion:** by default the app types Unicode input events one character at a time, about 100 per second. It does not modify the clipboard or synthesize an Enter shortcut. Typing stops if focus changes or Esc is pressed; the rest is retained for Ctrl+Alt+V, so remove the partial text first. With `insertion = "paste"` (**Pasting** in the window), Dictly puts the transcript on the clipboard, marked to be skipped by clipboard history where the OS supports it (Windows clipboard history and cloud sync, macOS concealed type, KDE Klipper), then sends Ctrl+V (Command+V on macOS) and restores the previous clipboard text after 400 ms. A target that reads the clipboard later than that may paste the restored text instead. Paste mode types instead when the clipboard holds anything other than plain text, such as images, files, or rich text, because those cannot be restored. On macOS and Linux only the clipboard's text is saved and restored. Before either method, line breaks (CR LF or a lone CR) become LF, and bidirectional controls (U+202A–U+202E, U+2066–U+2069) and zero-width characters (U+200B, U+2060, U+FEFF) are removed; the zero-width joiner used in emoji and the zero-width non-joiner used in Persian and Indic spelling stay. Any other control character except tab blocks insertion. A typed line break was checked not to trigger Enter in a Tk text field. Whether a typed or pasted line break acts as Enter in consoles, Windows Terminal, or Chromium and Electron chat inputs (Slack, Discord, browser forms) is not verified, so it may send a message or run a command there. Multiline text and Unicode support depend on the receiving app. Windows accepting the events does not prove the target inserted them. Insertion is capped at 16,000 UTF-16 code units; longer transcripts remain retained but cannot be inserted through the recovery shortcut.
- **AI output:** transcription and cleanup can be wrong. Cleanup instructions ask the model to preserve meaning and treat dictated instructions as text, but do not guarantee either. Check the output before sending or acting on it.
- **Missing features:** no tray icon, configurable shortcuts, streaming transcription, snippets, app-specific styles, automatic dictionary learning, account sync, offline model, or installer. A compatible local provider must be installed separately.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| Console disappears after double-clicking | You may be running the old console-only build. Rebuild and open `target\release\dictly.exe`; the current executable opens a desktop window without arguments. |
| `Set the OPENAI_API_KEY environment variable` | Paste the key into the desktop API-key field, or set the variable in the same terminal used to launch a CLI command. Check `api_key_env` if customized. |
| `Speech provider returned HTTP 404 Not Found` | The provider has no `{base_url}/audio/transcriptions` endpoint. Check that `base_url` ends in the API prefix, such as `/v1`. For Vercel AI Gateway use exactly `https://ai-gateway.vercel.sh/v1`; other gateway hostnames or proxies in front of it are not recognized. |
| Window settings cannot be edited | Click **Disable dictation** first, then edit and re-enable. Enabling saves the settings for the next launch. |
| `Could not load configuration` in the window | The file is left unchanged and the defaults shown apply to this session only. Fix the reported error (`dictly --config <file> check` shows it) or remove the file, then restart Dictly. |
| Hotkeys do nothing while Dictly is focused | Focus a text field in another application; Dictly excludes its own settings fields from dictation. |
| Configuration error | Check spelling and value types. Unknown keys are rejected. Use `check` before `run`. |
| `saved as UTF-16` | The file was written as UTF-16, for example with PowerShell 5's `>` or `Out-File`. Save it as UTF-8, for example with `Set-Content -Encoding utf8`, or re-save it in Notepad as UTF-8. |
| `Dictly is already running` | Use the open Dictly window, or close it (or the console `run` session) first. |
| `No microphone found` or `Microphone "…" is not connected` | Connect a microphone, click **Refresh**, and choose it or **System default**. `dictly devices` lists the names Windows offers; an empty list means Windows reports no active input device. |
| No default microphone or access denied | Check desktop microphone permissions, device connection, and other apps holding the device. |
| Unsupported microphone sample format | The capture path accepts float32, signed 16-bit, or unsigned 16-bit samples. Try another default device or its supported Windows format settings. |
| Too short or no audible input | Speak for more than 200 ms; check mute, input level, and selected default microphone. |
| `needs Accessibility access` (macOS) | Allow the app that started Dictly in System Settings › Privacy & Security › Accessibility, then enable dictation again. For the command line that app is your terminal. |
| `needs an X11 session` (Linux) | Log out and choose an Xorg/X11 session at the login screen. |
| `Cannot save the API key in the system keyring` (Linux) | No Secret Service is running. Install and unlock GNOME Keyring or KWallet, or leave **Remember this key** unchecked. |
| Cannot register a shortcut | Close another Dictly instance or another app using Ctrl+Alt+Space, Ctrl+Alt+Esc, or Ctrl+Alt+V. |
| HTTP 401/403 | Check provider credentials, API access, and model permissions. |
| HTTP 404 | Check the API base URL and configured model. The app appends `/audio/transcriptions` and `/chat/completions`. |
| HTTP 429 or 5xx | Dictly already retried once. Check quota or provider availability and record again later. |
| Repeated characters in Notepad (`ssss…`) | Builds before 2026-10-16 typed the whole transcript in one batch. Update Dictly, or set `insertion = "paste"`. |
| Cleanup failed | The original transcript is used. The cause follows in parentheses: for an HTTP 401/403 check the key and model access; for a 404 or no/empty text confirm that the provider supports chat completions and the configured cleanup model. |
| Text retained or shortcut-release warning | Focus the intended field, press Ctrl+Alt+V, and release all shortcut/modifier keys promptly. |
| Recording stops when releasing the shortcut | That is hold-to-talk. Tap the shortcut (shorter than half a second) for hands-free recording. |
| No text or partial text despite insertion | Try non-elevated Notepad. Elevated apps, secure desktops, games, and some editors may reject Unicode input. Do not elevate the dictation app as a general workaround. |
| Still processing after cancel | Cancellation suppresses insertion, retries, and cleanup, not the request already in flight. Each API request has a 90-second timeout. |

When reporting a problem, include the OS and version, target app, microphone type, the non-sensitive error message, and whether cleanup was enabled. Exclude API keys, private audio, and transcripts unless you deliberately choose to share them.
