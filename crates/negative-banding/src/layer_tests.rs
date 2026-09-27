use super::*;
use std::{f64::consts::TAU, io::Write};

pub(crate) fn fixture(channels: usize, boost: f64, strength: f64) -> (Analysis, Vec<u16>) {
    let config = Config {
        compact_opacity: 1.0,
        residual_refine: false,
        width: 384,
        height: 64,
        channels,
        options: BandingOptions {
            strength,
            max_frequencies: 2,
            ..Default::default()
        },
        carrier_boost: boost,
    };
    let mut raw = Vec::new();
    for y in 0..config.height {
        for x in 0..config.width {
            for channel in 0..channels {
                let level = if y >= 56 {
                    0.8
                } else {
                    0.12 + 0.06 * channel as f64
                };
                let signal = (0.028 + 0.015 * y as f64 / 64.0)
                    * (TAU * x as f64 / (37.3 + channel as f64 * 0.9) + 0.3 * channel as f64).cos();
                let value = if x == 0 { 0.0 } else { level * signal.exp() };
                // Photoshop's native 16-bit samples expanded to UXP fullRange.
                raw.push((quantize(value) as f64 / 32768.0 * 65535.0).round_ties_even() as u16);
            }
        }
    }
    let mut file = tempfile::tempfile().unwrap();
    for value in &raw {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
    let analysis = analyze(&mut file, config, &AtomicBool::new(false)).unwrap();
    assert_eq!(analysis.layers.len(), 1);
    (analysis, raw)
}

#[test]
fn one_layer_and_one_shared_mask_reproduce_gray_and_rgb_reference() {
    for channels in [1, 3] {
        let (analysis, raw) = fixture(channels, 1.5, 0.8);
        let rendered = analysis.render(&raw, 0, &AtomicBool::new(false)).unwrap();
        let tile = &rendered.layers[0];
        let mut worst = 0u16;
        for (i, &value) in raw.iter().enumerate() {
            let base = quantize(value as f64 / 65535.0) as f64 / 32768.0;
            let signal = tile.pixels[i] as f64 / 32768.0;
            let mask = tile.mask[i / channels] as f64 / 32768.0;
            let blended = (base + 2.0 * signal - 1.0).clamp(0.0, 1.0);
            let actual = quantize(base + mask * (blended - base));
            let expected = quantize(rendered.reference[i] as f64 / 65535.0);
            worst = worst.max(actual.abs_diff(expected));
            if i / (channels * 384) >= 56 && (i / channels) % 384 != 0 {
                assert_eq!(tile.mask[i / channels], 0);
                assert_eq!(actual, quantize(value as f64 / 65535.0));
            }
        }
        assert!(
            worst <= 1,
            "maximum native Photoshop sample difference: {worst}"
        );
        assert!(tile.pixels.iter().any(|&v| v < 16384));
        assert!(tile.pixels.iter().any(|&v| v > 16384));
        assert!(tile.mask.iter().any(|&v| v > 0));
    }
}

#[test]
fn boost_changes_the_layer_and_mask_but_not_requested_result() {
    let (one, raw) = fixture(1, 1.0, 0.8);
    let (boosted, _) = fixture(1, 1.5, 0.8);
    let a = one.render(&raw, 0, &AtomicBool::new(false)).unwrap();
    let b = boosted.render(&raw, 0, &AtomicBool::new(false)).unwrap();
    assert_eq!(a.reference, b.reference);
    assert_ne!(a.layers[0].pixels, b.layers[0].pixels);
    assert_ne!(a.layers[0].mask, b.layers[0].mask);
    let y = 10;
    let strip = boosted
        .render(&raw[y * 384..(y + 2) * 384], y, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        strip.layers[0].pixels,
        b.layers[0].pixels[y * 384..(y + 2) * 384]
    );
    assert_eq!(
        strip.layers[0].mask,
        b.layers[0].mask[y * 384..(y + 2) * 384]
    );
}

#[test]
fn zero_strength_and_cancel_and_invalid_input() {
    let (analysis, raw) = fixture(1, 1.5, 0.0);
    let rendered = analysis.render(&raw, 0, &AtomicBool::new(false)).unwrap();
    assert_eq!(rendered.reference, raw);
    assert!(rendered.layers[0].mask.iter().all(|&v| v == 0));
    assert!(analysis.render(&raw, 0, &AtomicBool::new(true)).is_err());
    assert!(
        analysis
            .render(&raw[..4], 0, &AtomicBool::new(false))
            .is_err()
    );
    let mut bad = analysis.config.clone();
    bad.carrier_boost = f64::NAN;
    assert!(bad.validate().is_err());
    bad = analysis.config.clone();
    bad.options.detection_roi = Some([0, 200, 0, 8]);
    assert!(bad.validate().is_err());
}

#[test]
fn parallel_render_is_bit_identical_including_uneven_strips() {
    let (mut analysis, raw) = fixture(3, 1.5, 0.8);
    // Extend only the render bounds to exercise the threaded branch with the
    // same fitted field; edge extension is already part of the original model.
    analysis.config.height = 320;
    let source = raw.repeat(5);
    for (top, rows) in [(0, 320), (7, 303)] {
        let strip = &source[top * 384 * 3..(top + rows) * 384 * 3];
        let serial = analysis
            .render(strip, top, &AtomicBool::new(false))
            .unwrap();
        let parallel = analysis
            .render_parallel(strip, top, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(serial.reference, parallel.reference);
        assert_eq!(serial.layers[0].pixels, parallel.layers[0].pixels);
        assert_eq!(serial.layers[0].mask, parallel.layers[0].mask);
    }
    assert!(
        analysis
            .render_parallel(&source, 0, &AtomicBool::new(true))
            .is_err()
    );
    assert!(
        analysis
            .render_parallel(&source, usize::MAX, &AtomicBool::new(false))
            .is_err()
    );
}
