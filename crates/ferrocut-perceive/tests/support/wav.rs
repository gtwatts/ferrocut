//! Test fixtures: sine generator and a 16-bit PCM WAV writer.

use std::path::Path;

pub fn sine_stereo(rate: u32, freq: f64, dbfs: f64, secs: f64) -> Vec<f32> {
    let a = 10f64.powf(dbfs / 20.0);
    (0..(rate as f64 * secs) as usize)
        .flat_map(|i| {
            let v = (a * (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin()) as f32;
            [v, v]
        })
        .collect()
}

pub fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|&v| ((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect();
    let block = channels as u32 * 2;
    let mut b = Vec::with_capacity(44 + data.len());
    b.extend(b"RIFF");
    b.extend((36 + data.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(channels.to_le_bytes());
    b.extend(rate.to_le_bytes());
    b.extend((rate * block).to_le_bytes());
    b.extend((block as u16).to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    std::fs::write(path, b).unwrap();
}
