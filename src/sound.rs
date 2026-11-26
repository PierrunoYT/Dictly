//! Short tones that make the recording state audible while another app has focus.

#[derive(Clone, Copy)]
pub enum Cue {
    Start,
    Stop,
    Cancel,
    Error,
}

pub const RATE: u32 = 22_050;

/// A quiet sequence of 70 ms notes with a sine envelope, so there are no clicks.
/// Samples are in -1.0..=1.0 at `RATE`.
pub fn samples(cue: Cue) -> Vec<f32> {
    let notes: &[f32] = match cue {
        Cue::Start => &[660.0, 880.0],
        Cue::Stop => &[880.0, 660.0],
        Cue::Cancel => &[440.0],
        Cue::Error => &[330.0, 247.0],
    };
    let per_note = RATE as usize * 70 / 1000;
    let mut samples = Vec::with_capacity(per_note * notes.len());
    for &frequency in notes {
        for index in 0..per_note {
            let progress = index as f32 / per_note as f32;
            let envelope = (std::f32::consts::PI * progress).sin();
            let phase = std::f32::consts::TAU * frequency * index as f32 / RATE as f32;
            samples.push(phase.sin() * envelope * 0.2);
        }
    }
    samples
}
