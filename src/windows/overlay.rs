//! A small always-on-top status pill near the bottom of the target's monitor.
//! It never activates, takes keyboard focus, or receives mouse input, so the
//! focus checks that protect insertion are unaffected.
//!
//! A microphone badge sits on the left and a state icon on the right: a
//! running timer while recording, a spinner while transcribing, then a check,
//! warning, or cross. The pill is drawn at `SUPERSAMPLE` times its size and
//! scaled down, because GDI shapes are not anti-aliased.

use super::{Target, wide};
use crate::dictation::Tone;
use anyhow::{Result, ensure};
use std::{
    cell::RefCell,
    f32::consts::{FRAC_PI_2, PI, TAU},
    mem::zeroed,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        ANTIALIASED_QUALITY, Arc, BeginPaint, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
        CreateCompatibleDC, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush,
        DEFAULT_CHARSET, DEFAULT_PITCH, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT,
        DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FW_SEMIBOLD,
        FillRect, GetMonitorInfoW, GetStockObject, HALFTONE, HDC, InvalidateRect,
        MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, NULL_BRUSH, NULL_PEN,
        OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, Polyline, RoundRect, SRCCOPY, SelectObject,
        SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, SetWindowRgn, StretchBlt,
        TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::HiDpi::GetDpiForWindow,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, HWND_TOPMOST, KillTimer,
        LWA_ALPHA, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, WM_PAINT, WM_TIMER,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    },
};

const WIDTH: i32 = 400;
const HEIGHT: i32 = 48;
const SUPERSAMPLE: i32 = 3;
/// Repaint timer for the recording clock and the transcription spinner.
const ANIMATION: usize = 1;

const BACKGROUND: COLORREF = rgb(24, 28, 40);
const ERROR_BACKGROUND: COLORREF = rgb(62, 22, 32);
const TEXT: COLORREF = rgb(245, 246, 248);
const MUTED: COLORREF = rgb(160, 168, 186);
const WHITE: COLORREF = rgb(255, 255, 255);
const ACCENT: COLORREF = rgb(99, 102, 241);
const ACCENT_LIGHT: COLORREF = rgb(129, 140, 248);
const TRACK: COLORREF = rgb(55, 62, 82);
const RED: COLORREF = rgb(239, 68, 68);
const RED_DIM: COLORREF = rgb(185, 40, 48);
const AMBER: COLORREF = rgb(245, 182, 66);
const GREEN: COLORREF = rgb(34, 197, 94);
const GRAY: COLORREF = rgb(120, 128, 146);

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    red as u32 | (green as u32) << 8 | (blue as u32) << 16
}

/// Convert 96-DPI layout units to the overlay monitor's pixels.
fn px(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}

struct Content {
    text: Vec<u16>,
    tone: Tone,
    dpi: i32,
    /// When this tone started; drives the recording clock and spinner.
    since: Instant,
}

thread_local! {
    // The window procedure runs on the thread that owns the overlay.
    static CONTENT: RefCell<Option<Content>> = const { RefCell::new(None) };
}

pub struct Overlay(HWND);

impl Overlay {
    pub fn new() -> Result<Self> {
        let class = wide("DictlyOverlay");
        // SAFETY: class and window parameters are valid for the calls; the
        // class name outlives registration and creation.
        unsafe {
            let instance = GetModuleHandleW(null());
            let mut window_class: WNDCLASSW = zeroed();
            window_class.lpfnWndProc = Some(procedure);
            window_class.hInstance = instance;
            window_class.lpszClassName = class.as_ptr();
            // Registration fails harmlessly when a previous session registered it.
            RegisterClassW(&window_class);
            let window = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                class.as_ptr(),
                class.as_ptr(),
                WS_POPUP,
                0,
                0,
                WIDTH,
                HEIGHT,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            ensure!(!window.is_null(), "Cannot create the status overlay");
            SetLayeredWindowAttributes(window, 0, 240, LWA_ALPHA);
            Ok(Self(window))
        }
    }

    /// Show `text` in the `tone` style, centered above the taskbar on `near`'s
    /// monitor, or on the primary monitor without a target. Showing recording
    /// again while it is already shown keeps the running clock.
    pub fn show(&self, text: &str, tone: Tone, near: Option<Target>) {
        let near = near.map_or(null_mut(), |target| target.window);
        // SAFETY: handles are only passed to Windows; info has the required size.
        unsafe {
            let monitor = MonitorFromWindow(near, MONITOR_DEFAULTTOPRIMARY);
            let mut info: MONITORINFO = zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            let work = if GetMonitorInfoW(monitor, &mut info) != 0 {
                info.rcWork
            } else {
                RECT {
                    left: 0,
                    top: 0,
                    right: 800,
                    bottom: 600,
                }
            };
            // Move onto the target monitor first so its DPI applies.
            SetWindowPos(
                self.0,
                HWND_TOPMOST,
                work.left,
                work.top,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE,
            );
            let dpi = (GetDpiForWindow(self.0) as i32).max(96);
            let (width, height) = (px(WIDTH, dpi), px(HEIGHT, dpi));
            CONTENT.with(|content| {
                let mut content = content.borrow_mut();
                let since = match &*content {
                    Some(shown) if shown.tone == tone && tone == Tone::Recording => shown.since,
                    _ => Instant::now(),
                };
                *content = Some(Content {
                    text: text.encode_utf16().collect(),
                    tone,
                    dpi,
                    since,
                });
            });
            match tone {
                Tone::Recording => SetTimer(self.0, ANIMATION, 250, None),
                Tone::Working => SetTimer(self.0, ANIMATION, 40, None),
                _ => KillTimer(self.0, ANIMATION) as usize,
            };
            // The system owns the region after SetWindowRgn succeeds.
            SetWindowRgn(
                self.0,
                CreateRoundRectRgn(0, 0, width + 1, height + 1, height, height),
                1,
            );
            SetWindowPos(
                self.0,
                HWND_TOPMOST,
                (work.left + work.right - width) / 2,
                work.bottom - height - px(40, dpi),
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(self.0, null(), 0);
        }
    }

    pub fn hide(&self) {
        CONTENT.with(|content| content.borrow_mut().take());
        // SAFETY: the window belongs to this thread.
        unsafe {
            KillTimer(self.0, ANIMATION);
            ShowWindow(self.0, SW_HIDE);
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once.
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER => {
            // SAFETY: the window is valid while it receives messages.
            unsafe { InvalidateRect(window, null(), 0) };
            0
        }
        WM_PAINT => {
            // SAFETY: standard paint sequence into a memory bitmap that is
            // scaled onto the window; every GDI object created here is deleted.
            unsafe {
                let mut paint: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(window, &mut paint);
                let mut client: RECT = zeroed();
                GetClientRect(window, &mut client);
                CONTENT.with(|content| {
                    // Windows can paint before the first show.
                    let Some(content) = &*content.borrow() else {
                        return;
                    };
                    let (width, height) = (client.right, client.bottom);
                    let (large_width, large_height) = (width * SUPERSAMPLE, height * SUPERSAMPLE);
                    let memory = CreateCompatibleDC(dc);
                    let bitmap = CreateCompatibleBitmap(dc, large_width, large_height);
                    let previous = SelectObject(memory, bitmap);
                    let canvas = Canvas {
                        dc: memory,
                        dpi: content.dpi * SUPERSAMPLE,
                    };
                    draw(&canvas, large_width, large_height, content);
                    SetStretchBltMode(dc, HALFTONE);
                    SetBrushOrgEx(dc, 0, 0, null_mut());
                    StretchBlt(
                        dc,
                        0,
                        0,
                        width,
                        height,
                        memory,
                        0,
                        0,
                        large_width,
                        large_height,
                        SRCCOPY,
                    );
                    SelectObject(memory, previous);
                    DeleteObject(bitmap);
                    DeleteDC(memory);
                });
                EndPaint(window, &paint);
            }
            0
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn draw(canvas: &Canvas, width: i32, height: i32, content: &Content) {
    let p = |value| canvas.px(value);
    let error = content.tone == Tone::Error;
    canvas.fill(
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        if error { ERROR_BACKGROUND } else { BACKGROUND },
    );
    if error {
        let inset = p(1);
        canvas.shape(
            RECT {
                left: inset,
                top: inset,
                right: width - inset,
                bottom: height - inset,
            },
            height - 2 * inset,
            None,
            Some((p(2), RED)),
        );
    }
    let middle = height / 2;
    let elapsed = content.since.elapsed();
    let badge = match content.tone {
        // The badge pulses each half second while recording.
        Tone::Recording if elapsed.as_millis() / 500 % 2 == 1 => RED_DIM,
        Tone::Recording | Tone::Error => RED,
        _ => ACCENT,
    };
    canvas.disc(p(26), middle, p(17), badge);
    microphone(canvas, p(26), middle);
    canvas.text(
        &content.text,
        RECT {
            left: p(52),
            top: 0,
            right: width - p(58),
            bottom: height,
        },
        DT_LEFT,
        TEXT,
    );
    let icon = width - p(28);
    match content.tone {
        Tone::Recording => {
            let seconds = elapsed.as_secs();
            let clock: Vec<u16> = format!("{}:{:02}", seconds / 60, seconds % 60)
                .encode_utf16()
                .collect();
            let area = RECT {
                left: width - p(70),
                top: 0,
                right: width - p(18),
                bottom: height,
            };
            canvas.text(&clock, area, DT_RIGHT, MUTED);
        }
        Tone::Working => {
            // Arc::from == to draws the whole circle.
            canvas.arc(icon, middle, p(9), 0.0, 0.0, p(3), TRACK);
            let angle = -(elapsed.as_secs_f32() * TAU);
            canvas.arc(
                icon,
                middle,
                p(9),
                angle,
                angle + FRAC_PI_2,
                p(3),
                ACCENT_LIGHT,
            );
        }
        Tone::Success => {
            canvas.disc(icon, middle, p(11), GREEN);
            canvas.polyline(
                &[
                    POINT {
                        x: icon - p(5),
                        y: middle,
                    },
                    POINT {
                        x: icon - p(1),
                        y: middle + p(4),
                    },
                    POINT {
                        x: icon + p(5),
                        y: middle - p(4),
                    },
                ],
                p(2),
                WHITE,
            );
        }
        Tone::Warning => {
            canvas.disc(icon, middle, p(11), AMBER);
            exclamation(canvas, icon, middle, BACKGROUND);
        }
        Tone::Error => {
            canvas.disc(icon, middle, p(11), RED);
            exclamation(canvas, icon, middle, WHITE);
        }
        Tone::Neutral => {
            canvas.disc(icon, middle, p(11), GRAY);
            for direction in [1, -1] {
                canvas.polyline(
                    &[
                        POINT {
                            x: icon - p(4),
                            y: middle - direction * p(4),
                        },
                        POINT {
                            x: icon + p(4),
                            y: middle + direction * p(4),
                        },
                    ],
                    p(2),
                    WHITE,
                );
            }
        }
    }
}

fn microphone(canvas: &Canvas, x: i32, y: i32) {
    let p = |value| canvas.px(value);
    canvas.shape(
        RECT {
            left: x - p(4),
            top: y - p(10),
            right: x + p(4),
            bottom: y + p(3),
        },
        p(8),
        Some(WHITE),
        None,
    );
    // The lower half of a circle around the capsule.
    canvas.arc(x, y - p(1), p(7), PI, TAU, p(2), WHITE);
    canvas.polyline(
        &[POINT { x, y: y + p(6) }, POINT { x, y: y + p(9) }],
        p(2),
        WHITE,
    );
    canvas.polyline(
        &[
            POINT {
                x: x - p(4),
                y: y + p(9),
            },
            POINT {
                x: x + p(4),
                y: y + p(9),
            },
        ],
        p(2),
        WHITE,
    );
}

fn exclamation(canvas: &Canvas, x: i32, y: i32, color: COLORREF) {
    let p = |value| canvas.px(value);
    canvas.polyline(
        &[POINT { x, y: y - p(6) }, POINT { x, y: y + p(1) }],
        p(2),
        color,
    );
    canvas.disc(x, y + p(5), p(3) / 2, color);
}

/// Drawing helpers on a memory device context. GDI validates every handle it
/// is given, and each helper deselects and deletes the objects it creates.
struct Canvas {
    dc: HDC,
    dpi: i32,
}

impl Canvas {
    fn px(&self, value: i32) -> i32 {
        px(value, self.dpi)
    }

    fn fill(&self, area: RECT, color: COLORREF) {
        // SAFETY: see `Canvas`; the brush is deleted after use.
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(self.dc, &area, brush);
            DeleteObject(brush);
        }
    }

    /// Fill and/or outline a rounded rectangle; a circle when square with a
    /// radius equal to its side.
    fn shape(
        &self,
        area: RECT,
        radius: i32,
        fill: Option<COLORREF>,
        outline: Option<(i32, COLORREF)>,
    ) {
        // SAFETY: see `Canvas`; created objects are deselected, then deleted.
        unsafe {
            let brush = fill.map(|color| CreateSolidBrush(color));
            let pen = outline.map(|(width, color)| CreatePen(PS_SOLID, width, color));
            let previous_brush =
                SelectObject(self.dc, brush.unwrap_or_else(|| GetStockObject(NULL_BRUSH)));
            let previous_pen =
                SelectObject(self.dc, pen.unwrap_or_else(|| GetStockObject(NULL_PEN)));
            RoundRect(
                self.dc,
                area.left,
                area.top,
                area.right,
                area.bottom,
                radius,
                radius,
            );
            SelectObject(self.dc, previous_pen);
            SelectObject(self.dc, previous_brush);
            for object in [brush, pen].into_iter().flatten() {
                DeleteObject(object);
            }
        }
    }

    fn disc(&self, x: i32, y: i32, radius: i32, color: COLORREF) {
        let area = RECT {
            left: x - radius,
            top: y - radius,
            right: x + radius,
            bottom: y + radius,
        };
        self.shape(area, 2 * radius, Some(color), None);
    }

    fn polyline(&self, points: &[POINT], width: i32, color: COLORREF) {
        // SAFETY: see `Canvas`; points outlives the call.
        unsafe {
            let pen = CreatePen(PS_SOLID, width, color);
            let previous = SelectObject(self.dc, pen);
            Polyline(self.dc, points.as_ptr(), points.len() as i32);
            SelectObject(self.dc, previous);
            DeleteObject(pen);
        }
    }

    /// Counter-clockwise arc between two angles (radians, y up).
    #[allow(clippy::too_many_arguments)]
    fn arc(&self, x: i32, y: i32, radius: i32, from: f32, to: f32, width: i32, color: COLORREF) {
        let point = |angle: f32| {
            (
                x + (radius as f32 * angle.cos()).round() as i32,
                y - (radius as f32 * angle.sin()).round() as i32,
            )
        };
        let ((start_x, start_y), (end_x, end_y)) = (point(from), point(to));
        // SAFETY: see `Canvas`.
        unsafe {
            let pen = CreatePen(PS_SOLID, width, color);
            let previous = SelectObject(self.dc, pen);
            Arc(
                self.dc,
                x - radius,
                y - radius,
                x + radius,
                y + radius,
                start_x,
                start_y,
                end_x,
                end_y,
            );
            SelectObject(self.dc, previous);
            DeleteObject(pen);
        }
    }

    fn text(&self, text: &[u16], mut area: RECT, align: DRAW_TEXT_FORMAT, color: COLORREF) {
        // An empty slice's pointer is dangling and must not reach DrawTextW.
        if text.is_empty() {
            return;
        }
        let face = wide("Segoe UI");
        // SAFETY: see `Canvas`; face and text outlive the calls.
        unsafe {
            let font = CreateFontW(
                -self.px(15),
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                // ClearType's colour fringes would smear when scaled down.
                ANTIALIASED_QUALITY as u32,
                DEFAULT_PITCH as u32,
                face.as_ptr(),
            );
            let previous = SelectObject(self.dc, font);
            SetBkMode(self.dc, TRANSPARENT as i32);
            SetTextColor(self.dc, color);
            DrawTextW(
                self.dc,
                text.as_ptr(),
                text.len() as i32,
                &mut area,
                align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
            );
            SelectObject(self.dc, previous);
            DeleteObject(font);
        }
    }
}
