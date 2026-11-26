use crate::sound::{Cue, RATE, samples};
use std::{io::Cursor, ptr::null_mut, sync::OnceLock};
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

pub fn play(cue: Cue) {
    static SOUNDS: OnceLock<[Vec<u8>; 4]> = OnceLock::new();
    let sounds = SOUNDS.get_or_init(|| {
        [
            wav(Cue::Start),
            wav(Cue::Stop),
            wav(Cue::Cancel),
            wav(Cue::Error),
        ]
    });
    let wav = &sounds[cue as usize];
    // SAFETY: SND_MEMORY reads a static WAV image that outlives asynchronous playback.
    unsafe {
        PlaySoundW(
            wav.as_ptr().cast(),
            null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

fn wav(cue: Cue) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .expect("in-memory WAV");
    for sample in samples(cue) {
        writer
            .write_sample((sample * i16::MAX as f32) as i16)
            .expect("in-memory WAV");
    }
    writer.finalize().expect("in-memory WAV");
    bytes.into_inner()
}
