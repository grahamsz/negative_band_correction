// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared-phase vertical band detection and spatial log-gain fitting.
//!
//! Coordinates always refer to source columns/rows; detection crops only move
//! the phase origin. DPI does not enter any numerical calculation.

use std::{
    f64::consts::{PI, TAU},
    sync::atomic::{AtomicBool, Ordering},
};

use nalgebra::{DMatrix, DVector};
use rustfft::{FftPlanner, num_complex::Complex64};
use serde::Serialize;

use super::BandingOptions;
use crate::error::{Error, Result};

pub struct Samples {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub max_value: f64,
    pub rows: Vec<usize>,
    pub held_rows: Vec<usize>,
    /// Per-channel, row-major original normalized samples, without log flooring.
    pub training: Vec<Vec<f64>>,
    pub held: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Frequency {
    pub frequency: f64,
    pub period_px: f64,
    pub amplitude: f64,
    pub phase: f64,
    pub coherence: f64,
    pub unit_coherence: f64,
    pub prominence: f64,
    pub validation_phase_difference: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Validation {
    pub period_px: f64,
    pub before: f64,
    pub after: f64,
    pub reduction_db: f64,
    pub strip_rms_before: f64,
    pub strip_rms_after: f64,
    pub strip_rms_reduction_db: f64,
    pub residual_along_original: f64,
    pub coherent_phase_reversed: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct ChannelModel {
    pub frequencies: Vec<Frequency>,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    /// One row-major (grid-y, grid-x) plane per component.
    pub amplitude: Vec<Vec<f64>>,
    pub confidence: Vec<Vec<f64>>,
    pub capped_tiles: Vec<usize>,
    pub window_pixels: usize,
    pub validation: Vec<Validation>,
    pub detection_roi_validation: Vec<Validation>,
    #[serde(skip)]
    along_x: Vec<Vec<f64>>,
    #[serde(skip)]
    carriers: Vec<Vec<f64>>,
}

#[derive(Debug, Serialize)]
pub struct Model {
    pub width: usize,
    pub height: usize,
    pub max_value: f64,
    pub channels: Vec<ChannelModel>,
}

impl Model {
    pub fn has_components(&self) -> bool {
        self.channels
            .iter()
            .any(|channel| !channel.frequencies.is_empty())
    }

    pub fn correction(&self, channel: usize, x: usize, y: usize) -> f64 {
        self.channels[channel].correction(x, y)
    }

    /// Reuse a caller-owned full-width row buffer during streaming output.
    pub fn correction_row(&self, channel: usize, y: usize, output: &mut [f64]) {
        assert_eq!(output.len(), self.width);
        let field = &self.channels[channel];
        output.fill(0.0);
        if field.frequencies.is_empty() {
            return;
        }
        let (lo, hi, fraction) = bracket(&field.y, y as f64);
        for (values, carrier) in field.along_x.iter().zip(&field.carriers) {
            let low = &values[lo * self.width..(lo + 1) * self.width];
            let high = &values[hi * self.width..(hi + 1) * self.width];
            for ((result, (&a, &b)), &cosine) in
                output.iter_mut().zip(low.iter().zip(high)).zip(carrier)
            {
                *result += (a + (b - a) * fraction) * cosine;
            }
        }
    }
}

impl ChannelModel {
    fn correction(&self, x: usize, y: usize) -> f64 {
        if self.frequencies.is_empty() {
            return 0.0;
        }
        let (x0, x1, fx) = bracket(&self.x, x as f64);
        let (y0, y1, fy) = bracket(&self.y, y as f64);
        let nx = self.x.len();
        self.frequencies
            .iter()
            .zip(&self.amplitude)
            .map(|(frequency, values)| {
                let a = values[y0 * nx + x0] + fx * (values[y0 * nx + x1] - values[y0 * nx + x0]);
                let b = values[y1 * nx + x0] + fx * (values[y1 * nx + x1] - values[y1 * nx + x0]);
                (a + fy * (b - a)) * (TAU * frequency.frequency * x as f64 + frequency.phase).cos()
            })
            .sum()
    }

    fn prepare_rows(&mut self, width: usize) {
        for (frequency, values) in self.frequencies.iter().zip(&self.amplitude) {
            let mut interpolated = Vec::with_capacity(width * self.y.len());
            for line in values.chunks_exact(self.x.len()) {
                for x in 0..width {
                    let (lo, hi, f) = bracket(&self.x, x as f64);
                    interpolated.push(line[lo] + f * (line[hi] - line[lo]));
                }
            }
            self.along_x.push(interpolated);
            self.carriers.push(
                (0..width)
                    .map(|x| (TAU * frequency.frequency * x as f64 + frequency.phase).cos())
                    .collect(),
            );
        }
    }
}

pub fn dark_weight(brightness: f64, full: f64, off: f64) -> f64 {
    let t = ((brightness - full) / (off - full)).clamp(0.0, 1.0);
    (1.0 - t * t * t * (10.0 - 15.0 * t + 6.0 * t * t)).clamp(0.0, 1.0)
}

pub(super) fn corrected_sample(
    original: u16,
    max: u16,
    correction: f64,
    mask: f64,
    strength: f64,
) -> u16 {
    if original == 0 || mask == 0.0 || strength == 0.0 || correction == 0.0 {
        return original;
    }
    // Keeping division and final scaling matches the reference quantization.
    let raw = f64::from(original) / f64::from(max);
    let value = (raw * (-strength * mask * correction).exp()).clamp(0.0, 1.0);
    (value * f64::from(max)).round_ties_even() as u16
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn validate(samples: &Samples, options: &BandingOptions) -> Result<()> {
    if samples.width < 3
        || samples.height < 16
        || !matches!(samples.channels, 1 | 3)
        || !matches!(samples.max_value, 255.0 | 65535.0)
    {
        return Err(invalid(
            "band correction requires at least 16 rows of 8/16-bit gray or RGB samples",
        ));
    }
    if !options.strength.is_finite()
        || !(0.0..=1.0).contains(&options.strength)
        || !options.dark_full.is_finite()
        || !options.dark_off.is_finite()
        || !(0.0 <= options.dark_full
            && options.dark_full < options.dark_off
            && options.dark_off <= 1.0)
        || !(1..=8).contains(&options.max_frequencies)
        || !options.window_cycles.is_finite()
        || options.window_cycles < 3.0
        || options.grid_y < 2
    {
        return Err(invalid(
            "invalid band strength, darkness thresholds, component count, or map settings",
        ));
    }
    let roi = options
        .detection_roi
        .unwrap_or([0, samples.width as u32, 0, samples.height as u32]);
    if !(roi[0] < roi[1]
        && roi[1] as usize <= samples.width
        && roi[2] < roi[3]
        && roi[3] as usize <= samples.height)
    {
        return Err(invalid(
            "band detection ROI must lie within the source image",
        ));
    }
    let width = (roi[1] - roi[0]) as f64;
    let maximum = options.max_period.unwrap_or(width / 5.0);
    if !options.min_period.is_finite()
        || !maximum.is_finite()
        || !(2.0 <= options.min_period && options.min_period < maximum && maximum <= width / 3.0)
    {
        return Err(invalid(
            "band period bounds must satisfy 2 <= min < max <= detection width/3",
        ));
    }
    for (rows, channels) in [
        (&samples.rows, &samples.training),
        (&samples.held_rows, &samples.held),
    ] {
        if rows.len() < 8
            || channels.len() != samples.channels
            || rows.windows(2).any(|pair| pair[0] >= pair[1])
            || rows.iter().any(|&y| y >= samples.height)
        {
            return Err(invalid("invalid band sampling rows or channel count"));
        }
        if rows
            .iter()
            .filter(|&&y| y >= roi[2] as usize && y < roi[3] as usize)
            .count()
            < 8
        {
            return Err(invalid(
                "band detection ROI needs eight training and eight held-out rows",
            ));
        }
        let expected = rows
            .len()
            .checked_mul(samples.width)
            .ok_or_else(|| invalid("band sample dimensions overflow"))?;
        if channels.iter().any(|channel| {
            channel.len() != expected
                || channel
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        }) {
            return Err(invalid(
                "band samples have invalid dimensions or nonfinite/out-of-range values",
            ));
        }
    }
    if samples
        .rows
        .iter()
        .any(|row| samples.held_rows.binary_search(row).is_ok())
    {
        return Err(invalid(
            "band training and validation rows must be disjoint",
        ));
    }
    Ok(())
}

pub(super) fn analyze(
    samples: &Samples,
    options: &BandingOptions,
    cancel: &AtomicBool,
) -> Result<Model> {
    cancelled(cancel)?;
    validate(samples, options)?;
    let roi = options
        .detection_roi
        .unwrap_or([0, samples.width as u32, 0, samples.height as u32]);
    let mut channels = Vec::with_capacity(samples.channels);
    for channel in 0..samples.channels {
        cancelled(cancel)?;
        let training = strip_profiles(
            &samples.training[channel],
            samples.width,
            &samples.rows,
            roi,
            samples.max_value,
        );
        let held = strip_profiles(
            &samples.held[channel],
            samples.width,
            &samples.held_rows,
            roi,
            samples.max_value,
        );
        let mut frequencies = detect(&training, &held, options, cancel)?;
        for item in &mut frequencies {
            item.phase = wrap_phase(item.phase - TAU * item.frequency * f64::from(roi[0]));
        }
        if frequencies.is_empty() {
            channels.push(ChannelModel::default());
            continue;
        }
        let mut model = fit_amplitude_maps(samples, channel, frequencies, options, cancel)?;
        let full_roi = [0, samples.width as u32, 0, samples.height as u32];
        model.validation = validation_report(samples, channel, &model, options, full_roi, cancel)?;
        if options.detection_roi.is_some() {
            model.detection_roi_validation =
                validation_report(samples, channel, &model, options, roi, cancel)?;
        }
        model.prepare_rows(samples.width);
        channels.push(model);
    }
    Ok(Model {
        width: samples.width,
        height: samples.height,
        max_value: samples.max_value,
        channels,
    })
}

fn median(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_unstable_by(f64::total_cmp);
    let n = ordered.len();
    if n == 0 {
        return 0.0;
    }
    if n.is_multiple_of(2) {
        (ordered[n / 2 - 1] + ordered[n / 2]) / 2.0
    } else {
        ordered[n / 2]
    }
}

fn linspace(first: f64, last: f64, count: usize) -> Vec<f64> {
    if count <= 1 {
        return vec![first];
    }
    (0..count)
        .map(|i| first + (last - first) * i as f64 / (count - 1) as f64)
        .collect()
}

fn bracket(centers: &[f64], position: f64) -> (usize, usize, f64) {
    if centers.len() == 1 {
        return (0, 0, 0.0);
    }
    let hi = centers
        .partition_point(|&value| value < position)
        .clamp(1, centers.len() - 1);
    let lo = hi - 1;
    (
        lo,
        hi,
        ((position - centers[lo]) / (centers[hi] - centers[lo])).clamp(0.0, 1.0),
    )
}

fn wrap_phase(phase: f64) -> f64 {
    (phase + PI).rem_euclid(TAU) - PI
}

fn strip_profiles(
    values: &[f64],
    source_width: usize,
    rows: &[usize],
    roi: [u32; 4],
    max: f64,
) -> Vec<Vec<f64>> {
    let indices: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|&(_, &y)| y >= roi[2] as usize && y < roi[3] as usize)
        .map(|(i, _)| i)
        .collect();
    let count = indices.len().min(16);
    let width = (roi[1] - roi[0]) as usize;
    let mut result = Vec::with_capacity(count);
    let mut start = 0;
    for group in 0..count {
        let length = indices.len() / count + usize::from(group < indices.len() % count);
        let mut profile = vec![0.0; width];
        for &row in &indices[start..start + length] {
            let first = row * source_width + roi[0] as usize;
            for (target, &value) in profile.iter_mut().zip(&values[first..first + width]) {
                *target += value.max(1.0 / max);
            }
        }
        for value in &mut profile {
            *value = (*value / length as f64).ln();
        }
        result.push(profile);
        start += length;
    }
    result
}

struct Spectrum {
    residual: Vec<Vec<f64>>,
    window: Vec<f64>,
    amplitude: Vec<f64>,
    coherence: Vec<f64>,
    unit_coherence: Vec<f64>,
}

fn least_squares(design: &DMatrix<f64>, target: &DVector<f64>) -> Result<DVector<f64>> {
    let svd = design.clone().svd(true, true);
    let largest = svd.singular_values.iter().copied().fold(0.0, f64::max);
    let tolerance = largest * f64::EPSILON * design.nrows().max(design.ncols()) as f64;
    if svd
        .singular_values
        .iter()
        .filter(|&&value| value > tolerance)
        .count()
        < design.ncols()
    {
        return Err(invalid(
            "band amplitude fit is rank deficient; widen the fitting window or separate its frequencies",
        ));
    }
    let answer = svd.solve(target, tolerance).map_err(invalid)?;
    if answer.iter().any(|v| !v.is_finite()) {
        return Err(invalid(
            "band least-squares solver produced a nonfinite coefficient",
        ));
    }
    Ok(answer)
}

fn spectral_data(profiles: &[Vec<f64>], transform: bool, cancel: &AtomicBool) -> Result<Spectrum> {
    let width = profiles[0].len();
    let baseline = DMatrix::from_fn(width, 4, |row, col| {
        (-1.0 + 2.0 * row as f64 / (width - 1) as f64).powi(col as i32)
    });
    let window: Vec<_> = (0..width)
        .map(|x| {
            let t = TAU * x as f64 / (width - 1) as f64;
            0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos()
        })
        .collect();
    let nfft = width
        .checked_mul(16)
        .ok_or_else(|| invalid("band FFT dimensions overflow"))?;
    let bins = if transform { nfft / 2 + 1 } else { 0 };
    let mut means = vec![Complex64::default(); bins];
    let mut magnitudes = vec![0.0; bins];
    let mut units = vec![Complex64::default(); bins];
    let mut residuals = Vec::with_capacity(profiles.len());
    let mut planner = FftPlanner::<f64>::new();
    let fft = if transform {
        Some(planner.plan_fft_forward(nfft))
    } else {
        None
    };
    let normalization = 2.0 / window.iter().sum::<f64>();
    for profile in profiles {
        cancelled(cancel)?;
        let target = DVector::from_column_slice(profile);
        let coefficients = least_squares(&baseline, &target)?;
        let residual: Vec<_> = (target - &baseline * coefficients)
            .iter()
            .copied()
            .collect();
        if let Some(fft) = &fft {
            let mut buffer = vec![Complex64::default(); nfft];
            for ((target, &value), &weight) in buffer.iter_mut().zip(&residual).zip(&window) {
                target.re = value * weight;
            }
            fft.process(&mut buffer);
            for i in 0..bins {
                let z = buffer[i] * normalization;
                let magnitude = z.norm();
                means[i] += z / profiles.len() as f64;
                magnitudes[i] += magnitude / profiles.len() as f64;
                units[i] += z / magnitude.max(1e-30) / profiles.len() as f64;
            }
        }
        residuals.push(residual);
    }
    let amplitude: Vec<_> = means.iter().map(|z| z.norm()).collect();
    let coherence = amplitude
        .iter()
        .zip(magnitudes)
        .map(|(a, m)| a / m.max(1e-30))
        .collect();
    let unit_coherence = units.iter().map(|z| z.norm()).collect();
    Ok(Spectrum {
        residual: residuals,
        window,
        amplitude,
        coherence,
        unit_coherence,
    })
}

fn phasors(spectrum: &Spectrum, frequency: f64) -> Vec<Complex64> {
    let reference: Vec<_> = spectrum
        .window
        .iter()
        .enumerate()
        .map(|(x, &weight)| Complex64::from_polar(weight, -TAU * frequency * x as f64))
        .collect();
    let normalization = 2.0 / spectrum.window.iter().sum::<f64>();
    spectrum
        .residual
        .iter()
        .map(|line| {
            line.iter()
                .zip(&reference)
                .map(|(value, reference)| reference * value)
                .sum::<Complex64>()
                * normalization
        })
        .collect()
}

fn complex_mean(values: &[Complex64]) -> Complex64 {
    values.iter().sum::<Complex64>() / values.len() as f64
}

fn detect(
    profiles: &[Vec<f64>],
    held: &[Vec<f64>],
    options: &BandingOptions,
    cancel: &AtomicBool,
) -> Result<Vec<Frequency>> {
    let width = profiles[0].len();
    let spectrum = spectral_data(profiles, true, cancel)?;
    let check = spectral_data(held, false, cancel)?;
    let minimum_frequency = 1.0 / options.max_period.unwrap_or(width as f64 / 5.0);
    let maximum_frequency = 1.0 / options.min_period;
    let numerical_floor = 64.0
        * f64::EPSILON
        * profiles
            .iter()
            .flatten()
            .map(|x| x.abs())
            .fold(1e-30, f64::max);
    let amplitude = &spectrum.amplitude;
    let mut candidates = Vec::new();
    for j in 1..amplitude.len() - 1 {
        let f = j as f64 / (16 * width) as f64;
        if f < minimum_frequency
            || f > maximum_frequency
            || amplitude[j] <= numerical_floor
            || amplitude[j] <= amplitude[j - 1]
            || amplitude[j] < amplitude[j + 1]
        {
            continue;
        }
        let flanks: Vec<_> = (j.saturating_sub(192).max(1)..j.saturating_sub(64).max(2))
            .chain(j + 64..(j + 192).min(amplitude.len()))
            .filter_map(|index| amplitude.get(index).copied())
            .collect();
        if flanks.is_empty() {
            continue;
        }
        let prominence = amplitude[j] / median(&flanks).max(1e-30);
        let coherence = spectrum.coherence[j];
        let unit = spectrum.unit_coherence[j];
        if coherence < 0.55 || unit < 0.5 || prominence < 4.5 {
            continue;
        }
        cancelled(cancel)?;
        let a = amplitude[j - 1].max(1e-30).ln();
        let b = amplitude[j].max(1e-30).ln();
        let c = amplitude[j + 1].max(1e-30).ln();
        let denominator = a - 2.0 * b + c;
        let delta = if denominator == 0.0 {
            0.0
        } else {
            (0.5 * (a - c) / denominator).clamp(-0.5, 0.5)
        };
        let frequency =
            ((j as f64 + delta) / (16 * width) as f64).clamp(minimum_frequency, maximum_frequency);
        let average = complex_mean(&phasors(&spectrum, frequency));
        let held = complex_mean(&phasors(&check, frequency));
        let phase_difference = (held * average.conj()).arg();
        if phase_difference.abs() > PI / 3.0 || held.norm() < 0.25 * average.norm() {
            continue;
        }
        candidates.push(Frequency {
            frequency,
            period_px: 1.0 / frequency,
            amplitude: average.norm(),
            phase: average.arg(),
            coherence,
            unit_coherence: unit,
            prominence,
            validation_phase_difference: phase_difference,
            score: amplitude[j] * unit * unit,
        });
    }
    candidates.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
    let mut selected: Vec<Frequency> = Vec::new();
    for candidate in candidates {
        let largest = selected
            .iter()
            .map(|item| item.amplitude)
            .fold(0.0, f64::max);
        if candidate.amplitude < 0.05 * largest {
            continue;
        }
        if selected
            .iter()
            .all(|item| (candidate.frequency - item.frequency).abs() >= 4.0 / width as f64)
        {
            selected.push(candidate);
        }
        if selected.len() >= options.max_frequencies {
            break;
        }
    }
    Ok(selected)
}

fn robust_fit(design: &DMatrix<f64>, target: &DVector<f64>, hann: &[f64]) -> Result<DVector<f64>> {
    let solve = |roots: &[f64]| {
        let weighted = DMatrix::from_fn(design.nrows(), design.ncols(), |row, col| {
            design[(row, col)] * roots[row]
        });
        let target = DVector::from_fn(target.len(), |row, _| target[row] * roots[row]);
        least_squares(&weighted, &target)
    };
    let mut coefficients = solve(&hann.iter().map(|w| w.sqrt()).collect::<Vec<_>>())?;
    for _ in 0..3 {
        let residual = target - design * &coefficients;
        let scale = 1.4826 * median(&residual.iter().map(|r| r.abs()).collect::<Vec<_>>()) + 1e-12;
        let roots: Vec<_> = hann
            .iter()
            .zip(residual.iter())
            .map(|(w, r)| (w / (1.0 + (r / (2.0 * scale)).powi(2))).sqrt())
            .collect();
        coefficients = solve(&roots)?;
    }
    Ok(coefficients)
}

fn fit_amplitude_maps(
    samples: &Samples,
    channel: usize,
    frequencies: Vec<Frequency>,
    options: &BandingOptions,
    cancel: &AtomicBool,
) -> Result<ChannelModel> {
    let width = samples.width;
    let height = samples.height;
    let longest = frequencies
        .iter()
        .map(|item| item.period_px)
        .fold(0.0, f64::max);
    let mut required = options.window_cycles * longest;
    for a in 0..frequencies.len() {
        for b in 0..a {
            required =
                required.max(2.0 / (frequencies[a].frequency - frequencies[b].frequency).abs());
        }
    }
    let span = width.min(required.ceil() as usize);
    if (span as f64) < 3.0 * longest {
        return Err(invalid(
            "band amplitude fit requires at least three complete periods",
        ));
    }
    let nx = (((width - span) as f64 / (span as f64 / 4.0)).ceil() as usize + 1).max(2);
    let mut centers_x = linspace(
        (span - 1) as f64 / 2.0,
        (width - 1) as f64 - (span - 1) as f64 / 2.0,
        nx,
    );
    centers_x.dedup();
    let centers_y = linspace(
        0.0,
        (height - 1) as f64,
        options.grid_y.min(samples.rows.len()),
    );
    let spacings: Vec<_> = samples
        .rows
        .windows(2)
        .map(|p| (p[1] - p[0]) as f64)
        .collect();
    let radius = (1.5 * height as f64 / (options.grid_y - 1) as f64).max(2.0 * median(&spacings));
    let grid_len = centers_x.len() * centers_y.len();
    let count = frequencies.len();
    let mut field = ChannelModel {
        frequencies,
        x: centers_x,
        y: centers_y,
        amplitude: vec![vec![0.0; grid_len]; count],
        confidence: vec![vec![0.0; grid_len]; count],
        capped_tiles: vec![0; count],
        window_pixels: span,
        ..Default::default()
    };
    let logged: Vec<_> = samples.training[channel]
        .iter()
        .map(|v| v.max(1.0 / samples.max_value).ln())
        .collect();
    let hann: Vec<_> = (0..span)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / (span - 1) as f64).cos())
        .collect();
    // All tiles on a grid-x center share the design and original-Hann covariance.
    let mut designs = Vec::with_capacity(field.x.len());
    for &center in &field.x {
        cancelled(cancel)?;
        let first = (center - (span - 1) as f64 / 2.0)
            .round_ties_even()
            .clamp(0.0, (width - span) as f64) as usize;
        let design = DMatrix::from_fn(span, 3 + 2 * count, |row, col| {
            let u = -1.0 + 2.0 * row as f64 / (span - 1) as f64;
            if col < 3 {
                u.powi(col as i32)
            } else {
                let angle = TAU * field.frequencies[(col - 3) / 2].frequency * (first + row) as f64;
                if (col - 3) % 2 == 0 {
                    angle.cos()
                } else {
                    angle.sin()
                }
            }
        });
        let weighted = DMatrix::from_fn(span, design.ncols(), |row, col| {
            hann[row] * design[(row, col)]
        });
        let gram = design.transpose() * weighted;
        let largest = gram.norm();
        let covariance = gram
            .svd(true, true)
            .pseudo_inverse(1e-15 * largest)
            .map_err(invalid)?;
        designs.push((first, design, covariance));
    }
    for (iy, &center_y) in field.y.iter().enumerate() {
        cancelled(cancel)?;
        let mut profile = vec![0.0; width];
        let mut total_weight = 0.0;
        for (&row, line) in samples.rows.iter().zip(logged.chunks_exact(width)) {
            let weight = (1.0 - (row as f64 - center_y).abs() / radius).max(0.0);
            if weight > 0.0 {
                total_weight += weight;
                for (target, value) in profile.iter_mut().zip(line) {
                    *target += weight * value;
                }
            }
        }
        if total_weight <= 0.0 {
            return Err(invalid(
                "band amplitude grid has no supporting training rows",
            ));
        }
        for value in &mut profile {
            *value /= total_weight;
        }
        for (ix, (first, design, covariance)) in designs.iter().enumerate() {
            cancelled(cancel)?;
            let target = DVector::from_column_slice(&profile[*first..*first + span]);
            let coefficients = robust_fit(design, &target, &hann)?;
            let residual = target - design * &coefficients;
            let center = median(residual.as_slice());
            let noise = 1.4826
                * median(
                    &residual
                        .iter()
                        .map(|r| (r - center).abs())
                        .collect::<Vec<_>>(),
                );
            for (k, item) in field.frequencies.iter().enumerate() {
                let (sin, cos) = item.phase.sin_cos();
                let i = 3 + 2 * k;
                let parallel = coefficients[i] * cos - coefficients[i + 1] * sin;
                let quadrature = coefficients[i] * sin + coefficients[i + 1] * cos;
                let variance = cos * cos * covariance[(i, i)]
                    - cos * sin * (covariance[(i, i + 1)] + covariance[(i + 1, i)])
                    + sin * sin * covariance[(i + 1, i + 1)];
                let uncertainty = noise * variance.max(0.0).sqrt();
                let unwanted = quadrature * quadrature / 3.0 + (2.0 * uncertainty).powi(2);
                let support = (1.0 - unwanted / parallel.max(1e-30).powi(2)).clamp(0.0, 1.0);
                let amplitude = parallel.max(0.0) * support;
                let cap = (6.0 * item.amplitude).max(1e-12);
                if !amplitude.is_finite() || !support.is_finite() {
                    return Err(invalid("band amplitude estimate is nonfinite"));
                }
                field.capped_tiles[k] += usize::from(amplitude > cap);
                field.amplitude[k][iy * field.x.len() + ix] = amplitude.min(cap);
                field.confidence[k][iy * field.x.len() + ix] =
                    if parallel > 0.0 { support } else { 0.0 };
            }
        }
    }
    Ok(field)
}

fn validation_report(
    samples: &Samples,
    channel: usize,
    field: &ChannelModel,
    options: &BandingOptions,
    roi: [u32; 4],
    cancel: &AtomicBool,
) -> Result<Vec<Validation>> {
    let width = (roi[1] - roi[0]) as usize;
    let mut original = Vec::new();
    let mut corrected = Vec::new();
    let mut rows = Vec::new();
    for (index, &y) in samples.held_rows.iter().enumerate() {
        if y < roi[2] as usize || y >= roi[3] as usize {
            continue;
        }
        cancelled(cancel)?;
        rows.push(y);
        for x in roi[0] as usize..roi[1] as usize {
            let location = index * samples.width + x;
            let value = samples.held[channel][location];
            let brightness = samples
                .held
                .iter()
                .map(|values| values[location])
                .fold(0.0, f64::max);
            let mask = dark_weight(brightness, options.dark_full, options.dark_off);
            let integer = (value * samples.max_value).round_ties_even() as u16;
            let fixed = corrected_sample(
                integer,
                samples.max_value as u16,
                field.correction(x, y),
                mask,
                options.strength,
            );
            original.push(value);
            corrected.push(f64::from(fixed) / samples.max_value);
        }
    }
    let local_roi = [0, width as u32, roi[2], roi[3]];
    let before = spectral_data(
        &strip_profiles(&original, width, &rows, local_roi, samples.max_value),
        false,
        cancel,
    )?;
    let after = spectral_data(
        &strip_profiles(&corrected, width, &rows, local_roi, samples.max_value),
        false,
        cancel,
    )?;
    let mut result = Vec::new();
    for item in &field.frequencies {
        let z_before = phasors(&before, item.frequency);
        let z_after = phasors(&after, item.frequency);
        let z0 = complex_mean(&z_before);
        let z1 = complex_mean(&z_after);
        let rms0 =
            (z_before.iter().map(|z| z.norm_sqr()).sum::<f64>() / z_before.len() as f64).sqrt();
        let rms1 =
            (z_after.iter().map(|z| z.norm_sqr()).sum::<f64>() / z_after.len() as f64).sqrt();
        let projection = (z1 * z0.conj()).re;
        result.push(Validation {
            period_px: item.period_px,
            before: z0.norm(),
            after: z1.norm(),
            reduction_db: 20.0 * (z0.norm().max(1e-30) / z1.norm().max(1e-30)).log10(),
            strip_rms_before: rms0,
            strip_rms_after: rms1,
            strip_rms_reduction_db: 20.0 * (rms0.max(1e-30) / rms1.max(1e-30)).log10(),
            residual_along_original: projection / z0.norm_sqr().max(1e-30),
            coherent_phase_reversed: projection < 0.0,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(width: usize, height: usize, value: impl Fn(usize, usize) -> f64) -> Samples {
        let rows: Vec<_> = (0..height).step_by(2).collect();
        let held_rows: Vec<_> = (1..height).step_by(2).collect();
        let sample = |rows: &[usize]| {
            rows.iter()
                .flat_map(|&y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| {
                    (value(x, y) * 65535.0)
                        .round_ties_even()
                        .clamp(0.0, 65535.0)
                        / 65535.0
                })
                .collect()
        };
        Samples {
            width,
            height,
            channels: 1,
            max_value: 65535.0,
            training: vec![sample(&rows)],
            held: vec![sample(&held_rows)],
            rows,
            held_rows,
        }
    }

    fn options() -> BandingOptions {
        BandingOptions {
            strength: 0.8,
            dark_full: 0.1,
            dark_off: 0.6,
            max_frequencies: 1,
            min_period: 8.0,
            max_period: None,
            window_cycles: 4.0,
            grid_y: 5,
            detection_roi: None,
            save_raw: false,
            save_signal: false,
        }
    }

    #[test]
    fn mask_quantization_and_zero_protection() {
        assert_eq!(dark_weight(0.05, 0.1, 0.6), 1.0);
        assert!((dark_weight(0.35, 0.1, 0.6) - 0.5).abs() < 1e-15);
        assert_eq!(dark_weight(0.6, 0.1, 0.6), 0.0);
        for max in [255, 65535] {
            assert_eq!(corrected_sample(0, max, -100.0, 1.0, 0.8), 0);
            assert_eq!(corrected_sample(max, max, 0.1, 0.0, 0.8), max);
            assert_eq!(corrected_sample(17, max, 0.1, 1.0, 0.0), 17);
        }
        assert_eq!(
            corrected_sample(100, 255, (100.0f64 / 80.0).ln(), 1.0, 1.0),
            80
        );
    }

    #[test]
    fn flat_and_horizontal_images_have_no_vertical_carrier() {
        let cancel = AtomicBool::new(false);
        for horizontal in [false, true] {
            let source = samples(512, 64, |_, y| {
                if horizontal {
                    0.12 * (0.03 * (TAU * y as f64 / 12.0).cos()).exp()
                } else {
                    0.12
                }
            });
            assert!(
                !analyze(&source, &options(), &cancel)
                    .unwrap()
                    .has_components()
            );
        }
    }

    #[test]
    fn detects_vertical_carrier_and_reduces_held_out_band() {
        let source = samples(768, 96, |x, y| {
            let baseline = -2.8 + 0.08 * x as f64 / 768.0 + 0.1 * y as f64 / 96.0;
            let amplitude = 0.025 + 0.01 * y as f64 / 96.0;
            (baseline + amplitude * (TAU * x as f64 / 47.3 + 0.6).cos()).exp()
        });
        let model = analyze(&source, &options(), &AtomicBool::new(false)).unwrap();
        let frequency = &model.channels[0].frequencies[0];
        assert!((frequency.period_px - 47.3).abs() < 0.03, "{frequency:?}");
        assert!(wrap_phase(frequency.phase - 0.6).abs() < 0.04);
        assert!(model.channels[0].validation[0].reduction_db > 12.0);
        assert!(!model.channels[0].validation[0].coherent_phase_reversed);
        // Golden results from test3.py on this exact uint16 fixture: Fourier
        // peak refinement, local robust maps, and quantized held-row metrics.
        assert!((frequency.period_px - 47.300077305114236).abs() < 1e-8);
        assert!((frequency.phase - 0.6000882329740564).abs() < 1e-8);
        assert!((frequency.amplitude - 0.029894594383404808).abs() < 1e-10);
        assert!((model.channels[0].validation[0].reduction_db - 13.94646165582502).abs() < 1e-7);
        let expected_amplitudes = [
            0.0261826845264816,
            0.0261784704652124,
            0.0276264150025895,
            0.027620814637703,
            0.0299461412735728,
            0.0299426555152648,
            0.0322428484013,
            0.0322410678800018,
            0.0336443769016211,
            0.0336434662279755,
        ];
        for (&actual, expected) in model.channels[0].amplitude[0]
            .iter()
            .step_by(7)
            .zip(expected_amplitudes)
        {
            assert!(
                (actual - expected).abs() < 1e-8,
                "local amplitude {actual}, expected {expected}"
            );
        }
        let expected_corrections = [
            0.021608197525975,
            -0.0163907328733572,
            -0.0098113786359834,
            0.0241901736916583,
            -0.0183496842934658,
            -0.0109837834836536,
            0.0277662262245601,
            -0.0210622639758258,
            -0.0126053858691388,
        ];
        for ((x, y), expected) in [0, 41, 95]
            .into_iter()
            .flat_map(|y| [0, 107, 767].map(|x| (x, y)))
            .zip(expected_corrections)
        {
            assert!((model.correction(0, x, y) - expected).abs() < 1e-8);
        }
        let mut row = vec![0.0; source.width];
        model.correction_row(0, 41, &mut row);
        for (x, &value) in row.iter().enumerate() {
            assert!((value - model.correction(0, x, 41)).abs() < 1e-14);
        }
    }

    #[test]
    fn detection_roi_keeps_source_phase_origin() {
        let source = samples(1024, 96, |x, _| {
            (-2.9 + 0.03 * (TAU * x as f64 / 43.7 - 0.9).cos()).exp()
        });
        let mut opts = options();
        opts.detection_roi = Some([137, 900, 20, 80]);
        let model = analyze(&source, &opts, &AtomicBool::new(false)).unwrap();
        let component = &model.channels[0].frequencies[0];
        assert!((component.period_px - 43.7).abs() < 0.03);
        assert!(
            wrap_phase(component.phase + 0.9).abs() < 0.04,
            "{component:?}"
        );
        assert!(model.channels[0].detection_roi_validation[0].reduction_db > 12.0);
        for &(x, y) in &[(0, 0), (201, 45), (1000, 95)] {
            let expected = 0.03 * (TAU * x as f64 / 43.7 - 0.9).cos();
            assert!((model.correction(0, x, y) - expected).abs() < 0.001);
        }
    }

    #[test]
    fn fits_multiple_independent_carriers_jointly() {
        let source = samples(1024, 96, |x, y| {
            (-2.9
                + 0.08 * y as f64 / 96.0
                + 0.025 * (TAU * x as f64 / 47.3 + 0.6).cos()
                + 0.016 * (TAU * x as f64 / 71.8 - 1.1).cos())
            .exp()
        });
        let mut opts = options();
        opts.max_frequencies = 2;
        let model = analyze(&source, &opts, &AtomicBool::new(false)).unwrap();
        let channel = &model.channels[0];
        assert_eq!(channel.frequencies.len(), 2);
        for period in [47.3, 71.8] {
            assert!(
                channel
                    .frequencies
                    .iter()
                    .any(|f| (f.period_px - period).abs() < 0.05)
            );
        }
        assert!(
            channel
                .validation
                .iter()
                .all(|v| v.reduction_db > 12.0 && !v.coherent_phase_reversed)
        );
        assert!(
            channel
                .amplitude
                .iter()
                .flatten()
                .all(|a| a.is_finite() && *a >= 0.0)
        );
    }

    #[test]
    fn changing_strip_phases_are_not_a_shared_vertical_signal() {
        // Exercise phase agreement directly, before integer quantization can
        // introduce much smaller independent spectral lines.
        let profiles: Vec<Vec<f64>> = (0..16)
            .map(|strip| {
                let phase = TAU * strip as f64 / 16.0;
                (0..768)
                    .map(|x| -2.9 + 0.03 * (TAU * x as f64 / 43.7 + phase).cos())
                    .collect()
            })
            .collect();
        assert!(
            detect(&profiles, &profiles, &options(), &AtomicBool::new(false))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_changed_phase_on_validation_rows() {
        let source = samples(512, 64, |x, y| {
            (-2.9 + 0.03 * (TAU * x as f64 / 32.0 + if y % 2 == 0 { 0.0 } else { PI }).cos()).exp()
        });
        let mut opts = options();
        // Exclude tiny even harmonics introduced by integer quantization: those
        // legitimately keep the same phase when the fundamental reverses.
        opts.min_period = 24.0;
        assert!(
            !analyze(&source, &opts, &AtomicBool::new(false))
                .unwrap()
                .has_components()
        );
    }

    #[test]
    fn invalid_bounds_and_cancellation_are_explicit() {
        let source = samples(256, 32, |_, _| 0.1);
        assert!(matches!(
            analyze(&source, &options(), &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
        let mut opts = options();
        opts.dark_off = opts.dark_full;
        assert!(analyze(&source, &opts, &AtomicBool::new(false)).is_err());
        opts = options();
        opts.detection_roi = Some([0, 256, 1, 8]);
        assert!(analyze(&source, &opts, &AtomicBool::new(false)).is_err());
    }
}
