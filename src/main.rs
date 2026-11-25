#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use dictly::{audio::MAX_AUDIO_BYTES, config::Config, pipeline::Pipeline};
use std::{
    io::{IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    version,
    about = "Dictly desktop dictation. Launch without a command to open the Windows app."
)]
struct Args {
    /// TOML configuration file (desktop also checks beside the executable).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start Windows dictation: Ctrl+Alt+Space toggles, Ctrl+Alt+Esc cancels.
    Run,
    /// Transcribe an existing audio file and write text to stdout.
    Transcribe { file: PathBuf },
    /// Validate configuration without recording audio or contacting a provider.
    Check,
    /// List microphone names for the `microphone` setting (Windows).
    Devices,
}

fn main() -> ExitCode {
    // Windows GUI executables have no console on double-click. CLI invocations
    // attach to the calling terminal without creating a new console window.
    #[cfg(windows)]
    if std::env::args_os().len() > 1 {
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    let args = Args::parse();
    let desktop = args.command.is_none();
    let result = execute(args);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[cfg(windows)]
            if desktop {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    MB_ICONERROR, MB_OK, MessageBoxW,
                };
                let message: Vec<u16> = format!("{error:#}\0").encode_utf16().collect();
                let title: Vec<u16> = "Dictly\0".encode_utf16().collect();
                unsafe {
                    MessageBoxW(
                        std::ptr::null_mut(),
                        message.as_ptr(),
                        title.as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            #[cfg(not(windows))]
            let _ = desktop;
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: Args) -> Result<()> {
    let Some(command) = args.command else {
        #[cfg(windows)]
        return dictly::desktop::run(args.config);
        #[cfg(not(windows))]
        anyhow::bail!("The desktop app requires Windows. Use transcribe or check on this OS.");
    };
    let config = Config::load(args.config.as_deref())?;
    match command {
        Command::Check => {
            config.validate()?;
            println!("Configuration valid. No audio recorded or API requests made.");
        }
        Command::Transcribe { file } => {
            let mut bytes = Vec::new();
            std::fs::File::open(&file)
                .context("Cannot open audio file")?
                .take(MAX_AUDIO_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= MAX_AUDIO_BYTES, "Audio exceeds 24 MB");
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .context("Invalid filename")?;
            let result = Pipeline::new(config)?.transcribe(bytes, name)?;
            if result.cleanup_failed {
                eprintln!("Cleanup failed; returning the original transcript.");
            }
            // Provider text is untrusted: escape sequences could retitle, clear, or
            // write the clipboard of an interactive terminal. Redirected output stays raw.
            if std::io::stdout().is_terminal() {
                println!("{}", terminal_safe(&result.text));
            } else {
                println!("{}", result.text);
            }
        }
        Command::Devices => {
            #[cfg(windows)]
            for name in dictly::windows::microphones() {
                println!("{name}");
            }
            #[cfg(not(windows))]
            anyhow::bail!("Microphone listing requires Windows.");
        }
        Command::Run => {
            #[cfg(windows)]
            dictly::windows::run(config)?;
            #[cfg(not(windows))]
            anyhow::bail!("Live dictation requires Windows. Use transcribe or check on this OS.");
        }
    }
    Ok(())
}

/// Replace control characters other than line feed and tab with U+FFFD.
fn terminal_safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::terminal_safe;

    #[test]
    fn terminal_output_neutralizes_escape_sequences_but_keeps_lines_and_tabs() {
        assert_eq!(
            terminal_safe("hello \x1b]0;PWNED\x07\x1b[2J\u{9b}31m\r\nnext\tline\n"),
            "hello \u{fffd}]0;PWNED\u{fffd}\u{fffd}[2J\u{fffd}31m\u{fffd}\nnext\tline\n"
        );
        assert_eq!(terminal_safe("Grüße, 東京 👋"), "Grüße, 東京 👋");
    }
}
