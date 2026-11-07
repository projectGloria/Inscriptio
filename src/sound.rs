//! The "trimmed" chime: synthesized once in memory and played async via winmm.
//! No asset file: about 60 lines of math instead of a 40 KB .wav in the binary.

use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Mutex, OnceLock, PoisonError};
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

use crate::settings::VOLUME;

const SR: u32 = 44_100;

/// The chime at unit peak, synthesized once. Volume is applied on top.
static PCM: OnceLock<Vec<f32>> = OnceLock::new();

/// (volume it was rendered at, WAV bytes). SND_ASYNC returns immediately and
/// winmm keeps reading this buffer while it plays, so it lives in a static
/// and is only replaced after playback is stopped (see play()).
static WAV: Mutex<(u32, Vec<u8>)> = Mutex::new((u32::MAX, Vec::new()));

pub fn play() {
    let vol = VOLUME.load(Relaxed).min(100);
    if vol == 0 {
        return;
    }
    let mut cache = WAV.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.0 != vol {
        // Freeing the old buffer while winmm still streams it would be a
        // use-after-free. PlaySound(NULL) stops the current sound first.
        unsafe { PlaySoundW(null(), null_mut(), 0) };
        *cache = (vol, render(vol));
    }
    // SND_NODEFAULT: if anything fails, stay silent instead of the Windows "ding".
    unsafe { PlaySoundW(cache.1.as_ptr().cast(), null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
}

/// Re-rendering costs ~33k multiplies and only happens when the volume changes.
/// Scaling the samples ourselves instead of waveOutSetVolume, because that one
/// moves the app's slider in the Windows volume mixer as a side effect.
fn render(vol: u32) -> Vec<u8> {
    // Square law: loudness is perceived roughly logarithmically, so a linear
    // slider would do all its audible work in the bottom fifth. Max is -6 dBFS.
    let x = vol as f32 / 100.0;
    let g = 0.5 * x * x * i16::MAX as f32;
    let pcm: Vec<i16> = PCM.get_or_init(synth).iter().map(|&s| (s * g) as i16).collect();
    wav(&pcm)
}

/// Two soft plucks a perfect fifth apart (E5 → B5), each a warm
/// kalimba-ish tone: fundamental + a hint of 2nd/3rd harmonic, plus a copy
/// detuned by ~1.2 Hz whose slow beating gives the "vibe". One echo tap adds air.
fn synth() -> Vec<f32> {
    let len = (SR as f32 * 0.75) as usize;
    let mut buf = vec![0f32; len];
    let tau = std::f32::consts::TAU;

    for &(freq, start, decay, gain) in &[(659.25f32, 0.000f32, 0.20f32, 1.0f32), (987.77, 0.085, 0.26, 0.8)] {
        let s0 = (start * SR as f32) as usize;
        for (i, out) in buf[s0..].iter_mut().enumerate() {
            let t = i as f32 / SR as f32;
            // 5 ms attack ramp (no click), then exponential decay like a struck tine.
            let env = (t / 0.005).min(1.0) * (-t / decay).exp();
            let tone = (tau * freq * t).sin()
                + 0.6 * (tau * (freq + 1.2) * t).sin() // detuned twin → gentle chorus
                + 0.18 * (tau * 2.0 * freq * t).sin() * (-t / 0.06).exp() // bright "tick" fades fast
                + 0.05 * (tau * 3.0 * freq * t).sin();
            *out += gain * env * tone;
        }
    }

    // Single feedback-free echo at 120 ms, −12 dB: room feel without a reverb engine.
    let d = (0.12 * SR as f32) as usize;
    for i in (d..len).rev() {
        buf[i] += 0.25 * buf[i - d];
    }

    // Normalize to unit peak (render() applies the volume) and fade out the
    // last 40 ms so the buffer doesn't end on a non-zero sample (a click).
    let peak = buf.iter().fold(0f32, |m, &v| m.max(v.abs())).max(1e-6);
    let fade = (0.04 * SR as f32) as usize;
    buf.iter()
        .enumerate()
        .map(|(i, &v)| {
            let f = ((len - 1 - i) as f32 / fade as f32).min(1.0); // reaches exactly 0 on the last sample
            v / peak * f
        })
        .collect()
}

/// Minimal RIFF/WAVE container: PCM, mono, 16-bit.
fn wav(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&SR.to_le_bytes());
    v.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_is_well_formed() {
        let w = super::render(100);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
        assert_eq!(&w[36..40], b"data");
        // Last sample is silent: no end click.
        assert_eq!(i16::from_le_bytes(w[w.len() - 2..].try_into().unwrap()), 0);
    }

    #[test]
    fn volume_scales_peak() {
        let peak = |v| super::render(v)[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap();
        // Full volume peaks at about half scale (-6 dBFS); half volume at a quarter of that.
        assert!((16_000..=16_400).contains(&peak(100)));
        assert!((4_000..=4_200).contains(&peak(50)));
    }
}
