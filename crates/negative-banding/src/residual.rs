// SPDX-License-Identifier: MIT OR Apache-2.0
//! Signed, fixed-phase residual refinement for a single additive layer.
#[cfg(test)]
use crate::{Analysis, quantize};
use crate::{Config, Result, check_cancel, model};
use nalgebra::{DMatrix, DVector};
use serde::Serialize;
use std::{f64::consts::TAU, sync::atomic::AtomicBool};

#[derive(Clone)]
struct Field {
    x: Vec<f64>,
    y: Vec<f64>,
    amplitudes: Vec<Vec<f64>>,
    cosines: Vec<Vec<f64>>,
    sines: Vec<Vec<f64>>,
    span: usize,
}
fn bracket(grid: &[f64], v: f64) -> (usize, usize, f64) {
    let hi = grid.partition_point(|&p| p < v).min(grid.len() - 1);
    let lo = hi.saturating_sub(1);
    let f = if hi == lo {
        0.0
    } else {
        ((v - grid[lo]) / (grid[hi] - grid[lo])).clamp(0.0, 1.0)
    };
    (lo, hi, f)
}
impl Field {
    fn row(&self, y: usize, out: &mut [f64]) {
        self.region_row(0, y, out);
    }
    fn region_row(&self, origin_x: usize, y: usize, out: &mut [f64]) {
        out.fill(0.0);
        if self.amplitudes.is_empty() {
            return;
        }
        let (y0, y1, fy) = bracket(&self.y, y as f64);
        for (x, value) in out.iter_mut().enumerate() {
            let x = origin_x + x;
            let (x0, x1, fx) = bracket(&self.x, x as f64);
            for (a, cos) in self.amplitudes.iter().zip(&self.cosines) {
                let low = a[y0 * self.x.len() + x0] * (1.0 - fx) + a[y0 * self.x.len() + x1] * fx;
                let high = a[y1 * self.x.len() + x0] * (1.0 - fx) + a[y1 * self.x.len() + x1] * fx;
                *value += (low * (1.0 - fy) + high * fy) * cos[x];
            }
        }
    }
}
#[derive(Serialize)]
pub struct ChannelReport {
    pub channel: usize,
    pub grid_x: Vec<f64>,
    pub grid_y: Vec<f64>,
    pub refined_amplitudes: Vec<Vec<f64>>,
    pub accepted_iterations: usize,
    pub adjusted_nodes: usize,
    pub residual_rms_before: f64,
    pub residual_rms_after: f64,
    pub signed_before: Vec<Option<f64>>,
    pub signed_after: Vec<Option<f64>>,
}
pub struct Plan {
    fields: Vec<Field>,
    pub reports: Vec<ChannelReport>,
}
struct Profiles {
    original: Vec<Vec<f64>>,
    response: Vec<Vec<f64>>,
    corrected: Vec<Vec<f64>>,
}

// Response is dI/d(amplitude): original brightness * darkness * fixed carrier.
// Profiles use the ACTUAL interpolated map, not an isolated constant tile.
fn profiles(
    samples: &model::Samples,
    c: &Config,
    channel: usize,
    field: &Field,
    held: bool,
    cancel: &AtomicBool,
) -> Result<Profiles> {
    let (ys, channels) = if held {
        (&samples.held_rows, &samples.held)
    } else {
        (&samples.rows, &samples.training)
    };
    let w = c.width;
    let mut p = Profiles {
        original: vec![vec![0.0; w]; field.y.len()],
        response: vec![vec![0.0; w]; field.y.len()],
        corrected: vec![vec![0.0; w]; field.y.len()],
    };
    let radius = (1.5 * c.height as f64 / (field.y.len() - 1).max(1) as f64)
        .max(2.0 * c.height as f64 / ys.len() as f64);
    let mut totals = vec![0.0; field.y.len()];
    let mut correction = vec![0.0; w];
    for (r, &y) in ys.iter().enumerate() {
        check_cancel(cancel)?;
        field.row(y, &mut correction);
        let weights: Vec<_> = field
            .y
            .iter()
            .enumerate()
            .filter_map(|(i, &center)| {
                let weight = (1.0 - (y as f64 - center).abs() / radius).max(0.0);
                (weight > 0.0).then_some((i, weight))
            })
            .collect();
        for &(i, weight) in &weights {
            totals[i] += weight;
        }
        for (x, &signal) in correction.iter().enumerate() {
            let index = r * w + x;
            let raw = channels[channel][index];
            let bright = channels.iter().map(|v| v[index]).fold(0.0, f64::max);
            let response =
                raw * model::dark_weight(bright, c.options.dark_full, c.options.dark_off);
            let dark = if raw > 0.0 { response / raw } else { 0.0 };
            let corrected = raw + dark * ((raw * (1.0 - signal)).clamp(0.0, 1.0) - raw);
            for &(i, weight) in &weights {
                p.original[i][x] += weight * raw;
                p.response[i][x] += weight * response;
                p.corrected[i][x] += weight * corrected;
            }
        }
    }
    for (i, &total) in totals.iter().enumerate() {
        for data in [&mut p.original, &mut p.response, &mut p.corrected] {
            for v in &mut data[i] {
                *v /= total.max(1e-30);
            }
        }
    }
    Ok(p)
}
fn robust(design: &DMatrix<f64>, target: &DVector<f64>, weights: &[f64]) -> Option<DVector<f64>> {
    let solve = |weights: &[f64]| {
        let a = DMatrix::from_fn(design.nrows(), design.ncols(), |r, c| {
            design[(r, c)] * weights[r].sqrt()
        });
        let b = DVector::from_fn(target.len(), |r, _| target[r] * weights[r].sqrt());
        let svd = a.svd(true, true);
        let tolerance = svd.singular_values.iter().copied().fold(0.0, f64::max) * 1e-10;
        if svd.singular_values.iter().any(|&v| v <= tolerance) {
            return None;
        }
        svd.solve(&b, tolerance).ok()
    };
    let mut answer = solve(weights)?;
    for _ in 0..3 {
        let residual = target - design * &answer;
        let mut absolute: Vec<_> = residual.iter().map(|v| v.abs()).collect();
        absolute.sort_by(f64::total_cmp);
        let scale = 1.4826 * absolute[absolute.len() / 2] + 1e-12;
        let robust_weights: Vec<_> = weights
            .iter()
            .zip(residual.iter())
            .map(|(w, r)| w / (1.0 + (r / (2.0 * scale)).powi(2)))
            .collect();
        answer = solve(&robust_weights)?;
    }
    answer.iter().all(|v| v.is_finite()).then_some(answer)
}
#[derive(Clone, Copy)]
struct Residual {
    parallel: f64,
    quadrature: f64,
}
fn measure(p: &Profiles, field: &Field, cancel: &AtomicBool) -> Result<Vec<Vec<Option<Residual>>>> {
    let n = field.amplitudes.len();
    let mut result = vec![vec![None; field.x.len() * field.y.len()]; n];
    let hann: Vec<_> = (0..field.span)
        .map(|r| 0.5 - 0.5 * (TAU * r as f64 / (field.span - 1) as f64).cos())
        .collect();
    for iy in 0..field.y.len() {
        for (ix, &center) in field.x.iter().enumerate() {
            check_cancel(cancel)?;
            let first = (center - (field.span - 1) as f64 / 2.0)
                .round_ties_even()
                .clamp(0.0, (p.corrected[iy].len() - field.span) as f64)
                as usize;
            // Don't compensate away the protection of thin negatives.
            let response: f64 = p.response[iy][first..first + field.span].iter().sum();
            let level: f64 = p.original[iy][first..first + field.span].iter().sum();
            if response < 0.2 * level || response < 0.002 * field.span as f64 {
                continue;
            }
            let design = DMatrix::from_fn(field.span, 3 + 2 * n, |r, col| {
                if col < 3 {
                    (-1.0 + 2.0 * r as f64 / (field.span - 1) as f64).powi(col as i32)
                } else {
                    let k = (col - 3) / 2;
                    let x = first + r;
                    p.response[iy][x]
                        * if (col - 3) % 2 == 0 {
                            field.cosines[k][x]
                        } else {
                            field.sines[k][x]
                        }
                }
            });
            let target = DVector::from_column_slice(&p.corrected[iy][first..first + field.span]);
            if let Some(coef) = robust(&design, &target, &hann) {
                for k in 0..n {
                    result[k][iy * field.x.len() + ix] = Some(Residual {
                        parallel: coef[3 + 2 * k],
                        quadrature: coef[4 + 2 * k],
                    });
                }
            }
        }
    }
    Ok(result)
}
fn score(values: &[Vec<Option<Residual>>]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0;
    for v in values.iter().flatten().flatten() {
        sum += v.parallel.powi(2) + v.quadrature.powi(2);
        count += 1;
    }
    if count == 0 {
        0.0
    } else {
        (sum / count as f64).sqrt()
    }
}
fn signed(values: &[Vec<Option<Residual>>]) -> Vec<Option<f64>> {
    values
        .iter()
        .flatten()
        .map(|v| v.map(|v| v.parallel))
        .collect()
}
// Global RMS alone can hide a newly inverted patch behind improvements elsewhere.
// Compare every measurable held node after interpolation, including neighbours
// of the nodes whose amplitudes changed.
fn inversion_safe(
    before: &[Vec<Option<Residual>>],
    after: &[Vec<Option<Residual>>],
    base: &[Vec<f64>],
) -> bool {
    before
        .iter()
        .zip(after)
        .zip(base)
        .all(|((a, b), amplitudes)| {
            a.iter()
                .zip(b)
                .zip(amplitudes)
                .all(|((old, new), &amplitude)| match (old, new) {
                    (Some(old), Some(new)) => {
                        new.parallel >= old.parallel.min(0.0) - (0.01 * amplitude).max(1e-7)
                    }
                    (Some(_), None) => false,
                    _ => true,
                })
        })
}
impl Plan {
    pub(super) fn region_row(&self, channel: usize, x: usize, y: usize, out: &mut [f64]) {
        self.fields[channel].region_row(x, y, out);
    }
    pub(super) fn fit(
        samples: &model::Samples,
        c: &Config,
        model: &model::Model,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let mut fields = Vec::new();
        let mut reports = Vec::new();
        for (channel, base) in model.channels.iter().enumerate() {
            let mut field = Field {
                x: base.x.clone(),
                y: base.y.clone(),
                amplitudes: base.amplitude.clone(),
                span: base.window_pixels,
                cosines: base
                    .frequencies
                    .iter()
                    .map(|f| {
                        (0..c.width)
                            .map(|x| (TAU * f.frequency * x as f64 + f.phase).cos())
                            .collect()
                    })
                    .collect(),
                sines: base
                    .frequencies
                    .iter()
                    .map(|f| {
                        (0..c.width)
                            .map(|x| (TAU * f.frequency * x as f64 + f.phase).sin())
                            .collect()
                    })
                    .collect(),
            };
            if base.frequencies.is_empty() {
                fields.push(field);
                continue;
            }
            let mut held = measure(
                &profiles(samples, c, channel, &field, true, cancel)?,
                &field,
                cancel,
            )?;
            let mut report = ChannelReport {
                channel,
                grid_x: field.x.clone(),
                grid_y: field.y.clone(),
                refined_amplitudes: field.amplitudes.clone(),
                accepted_iterations: 0,
                adjusted_nodes: 0,
                residual_rms_before: score(&held),
                residual_rms_after: score(&held),
                signed_before: signed(&held),
                signed_after: signed(&held),
            };
            for _ in 0..8 {
                let train = measure(
                    &profiles(samples, c, channel, &field, false, cancel)?,
                    &field,
                    cancel,
                )?;
                let mut trial = field.clone();
                let mut changes = 0;
                for k in 0..base.frequencies.len() {
                    for j in 0..base.amplitude[k].len() {
                        let (Some(t), Some(h)) = (train[k][j], held[k][j]) else {
                            continue;
                        };
                        let original = base.amplitude[k][j];
                        let reducing = t.parallel < 0.0 && h.parallel < 0.0;
                        if original <= 1e-10
                            || (!reducing && base.confidence[k][j] < 0.25)
                            || t.parallel * h.parallel <= 0.0
                            || (t.parallel - h.parallel).abs()
                                > if reducing {
                                    0.75 * t
                                        .parallel
                                        .abs()
                                        .max(h.parallel.abs())
                                        .max(0.02 * original)
                                } else {
                                    0.5 * t.parallel.abs().max(0.02 * original)
                                }
                            || t.quadrature.abs().max(h.quadrature.abs())
                                > if reducing { original } else { 0.5 * original }
                        {
                            continue;
                        }
                        // Back off confident inverse-phase residuals more fully;
                        // increases stay cautious to avoid creating new inversions.
                        let step = if reducing { 0.95 } else { 0.5 };
                        let delta =
                            t.parallel.signum() * step * t.parallel.abs().min(h.parallel.abs());
                        let cap = (2.5 * original).min(6.0 * base.frequencies[k].amplitude);
                        let value = (field.amplitudes[k][j] + delta).clamp(0.0, cap);
                        if (value - field.amplitudes[k][j]).abs() > 1e-8 {
                            changes += 1;
                        }
                        trial.amplitudes[k][j] = value;
                    }
                }
                if changes == 0 {
                    break;
                }
                let mut accepted = None;
                let full_trial = trial.clone();
                for attempt in 0..10 {
                    if attempt == 5 {
                        // A risky increase must not veto supported reductions.
                        trial = full_trial.clone();
                        for (next, current) in trial
                            .amplitudes
                            .iter_mut()
                            .flatten()
                            .zip(field.amplitudes.iter().flatten())
                        {
                            *next = next.min(*current);
                        }
                    }
                    let measured = measure(
                        &profiles(samples, c, channel, &trial, true, cancel)?,
                        &trial,
                        cancel,
                    )?;
                    if score(&measured) < score(&held) * (1.0 - 1e-5)
                        && inversion_safe(&held, &measured, &base.amplitude)
                    {
                        accepted = Some(measured);
                        break;
                    }
                    // Reduce the entire step and remeasure the interpolated map.
                    for (next, current) in trial
                        .amplitudes
                        .iter_mut()
                        .flatten()
                        .zip(field.amplitudes.iter().flatten())
                    {
                        *next = 0.5 * (*next + current);
                    }
                }
                let Some(measured) = accepted else {
                    break;
                };
                let applied_changes = trial
                    .amplitudes
                    .iter()
                    .flatten()
                    .zip(field.amplitudes.iter().flatten())
                    .filter(|(a, b)| (*a - *b).abs() > 1e-8)
                    .count();
                field = trial;
                held = measured;
                report.accepted_iterations += 1;
                report.adjusted_nodes += applied_changes;
            }
            report.residual_rms_after = score(&held);
            report.signed_after = signed(&held);
            report.refined_amplitudes = field.amplitudes.clone();
            reports.push(report);
            fields.push(field);
        }
        Ok(Self { fields, reports })
    }
}
#[cfg(test)]
impl Analysis {
    pub fn render_adaptive_parallel(
        &self,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<(crate::Tile, Vec<u16>)> {
        let row = self.config.width * self.config.channels;
        if source.is_empty()
            || !source.len().is_multiple_of(row)
            || top
                .checked_add(source.len() / row)
                .is_none_or(|v| v > self.config.height)
        {
            return Err(crate::Error::Invalid(
                "Invalid adaptive strip bounds".into(),
            ));
        }
        let rows = source.len() / row;
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
            .min(rows);
        if workers < 2 || source.len() < 262_144 {
            return self.render_adaptive(source, top, cancel);
        }
        let step = rows.div_ceil(workers);
        let parts = std::thread::scope(|scope| {
            let handles: Vec<_> = source
                .chunks(step * row)
                .enumerate()
                .map(|(i, part)| {
                    scope.spawn(move || self.render_adaptive(part, top + i * step, cancel))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(crate::Error::Invalid(
                            "Adaptive render worker failed".into(),
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let mut tile = crate::Tile {
            pixels: Vec::with_capacity(source.len()),
            mask: Vec::with_capacity(source.len() / self.config.channels),
        };
        let mut reference = Vec::with_capacity(source.len());
        for (mut part, mut result) in parts {
            tile.pixels.append(&mut part.pixels);
            tile.mask.append(&mut part.mask);
            reference.append(&mut result);
        }
        Ok((tile, reference))
    }
    /// Combine locally refined waves in one neutral-centered Linear Light
    /// layer, retaining darkness as a separate editable pixel mask.
    pub fn render_adaptive(
        &self,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<(crate::Tile, Vec<u16>)> {
        let c = &self.config;
        let plan = self
            .residual
            .as_ref()
            .ok_or_else(|| crate::Error::Invalid("Residual refinement was not requested".into()))?;
        let row = c.width * c.channels;
        if source.is_empty()
            || !source.len().is_multiple_of(row)
            || top
                .checked_add(source.len() / row)
                .is_none_or(|v| v > c.height)
        {
            return Err(crate::Error::Invalid(
                "Invalid adaptive strip bounds".into(),
            ));
        }
        let mut layer = crate::Tile {
            pixels: vec![16384; source.len()],
            mask: vec![0; source.len() / c.channels],
        };
        let mut reference = source.to_vec();
        let mut signals = vec![vec![0.0; c.width]; c.channels];
        for (r, raw) in source.chunks_exact(row).enumerate() {
            check_cancel(cancel)?;
            for (field, signal) in plan.fields.iter().zip(&mut signals) {
                field.row(top + r, signal);
            }
            for (x, pixel) in raw.chunks_exact(c.channels).enumerate() {
                let bright = *pixel.iter().max().unwrap() as f64 / 65535.0;
                let dark = model::dark_weight(bright, c.options.dark_full, c.options.dark_off);
                layer.mask[r * c.width + x] = quantize(dark);
                let mask = layer.mask[r * c.width + x] as f64 / 32768.0;
                for channel in 0..c.channels {
                    let i = r * row + x * c.channels + channel;
                    let original = pixel[channel] as f64 / 65535.0;
                    let target = (original * (1.0 - c.options.strength * signals[channel][x]))
                        .clamp(0.0, 1.0);
                    layer.pixels[i] = quantize(0.5 + 0.5 * (target - original));
                    // Predict Photoshop's native 16-bit input and layer blend.
                    let base = quantize(original) as f64 / 32768.0;
                    let full =
                        (base + 2.0 * layer.pixels[i] as f64 / 32768.0 - 1.0).clamp(0.0, 1.0);
                    let blended = quantize(base + mask * (full - base));
                    reference[i] = (blended as f64 / 32768.0 * 65535.0).round_ties_even() as u16;
                }
            }
        }
        Ok((layer, reference))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BandingOptions;

    fn sample() -> (Config, model::Samples, Vec<u16>) {
        let c = Config {
            compact_opacity: 1.0,
            residual_refine: true,
            width: 384,
            height: 64,
            channels: 1,
            options: BandingOptions {
                strength: 1.0,
                max_frequencies: 1,
                ..Default::default()
            },
            carrier_boost: 1.5,
        };
        let mut training = Vec::new();
        let mut held = Vec::new();
        let mut raw = Vec::new();
        for y in 0..c.height {
            for x in 0..c.width {
                let amplitude = 0.035 + 0.012 * y as f64 / c.height as f64;
                let level = 0.06 + 0.002 * x as f64 / c.width as f64;
                let value = level * (amplitude * (TAU * x as f64 / 37.3 + 0.4).cos()).exp();
                raw.push((quantize(value) as f64 / 32768.0 * 65535.0).round_ties_even() as u16);
                if y % 2 == 0 {
                    training.push(value);
                } else {
                    held.push(value);
                }
            }
        }
        let samples = model::Samples {
            width: c.width,
            height: c.height,
            channels: 1,
            max_value: 65535.0,
            rows: (0..64).step_by(2).collect(),
            held_rows: (1..64).step_by(2).collect(),
            training: vec![training],
            held: vec![held],
        };
        (c, samples, raw)
    }
    #[test]
    fn full_precision_tiff_and_photoshop_use_the_same_refined_signal() {
        let (mut c, samples, raw) = sample();
        c.compact_opacity = 168.0 / 255.0;
        let cancel = AtomicBool::new(false);
        let analysis = crate::analyze_samples(samples, c, &cancel).unwrap();
        let (_, photoshop) = analysis.render_compact(None, &raw, 0, &cancel).unwrap();
        let mut odd_samples = 0;
        for y in 0..analysis.config.height {
            let mut signal = vec![0.0; analysis.config.width];
            analysis
                .correction_region_row(0, 0, y, &mut signal)
                .unwrap();
            for (x, &gain) in signal.iter().enumerate() {
                let index = y * analysis.config.width + x;
                let weight = crate::dark_weight(raw[index] as f64 / 65535.0, 0.1, 0.6);
                let tiff = crate::linear_corrected_sample(raw[index], 65535, gain, weight, 1.0);
                assert!(tiff.abs_diff(photoshop[index]) <= 6);
                odd_samples += usize::from(tiff % 2 == 1);
            }
            let mut crop = vec![0.0; 77];
            analysis.correction_region_row(0, 31, y, &mut crop).unwrap();
            assert_eq!(crop, &signal[31..108]);
        }
        assert!(
            odd_samples > raw.len() / 4,
            "TIFF export must not collapse to Photoshop's sample grid"
        );
        assert!(
            analysis
                .correction_region_row(0, analysis.config.width, 0, &mut [0.0])
                .is_err()
        );
    }
    #[test]
    fn signed_residual_search_corrects_under_and_over_application() {
        let (c, samples, _) = sample();
        let cancel = AtomicBool::new(false);
        for factor in [0.5, 1.7] {
            let mut base = model::analyze(&samples, &c.options, &cancel).unwrap();
            assert!(!base.channels[0].frequencies.is_empty());
            for a in base.channels[0].amplitude.iter_mut().flatten() {
                *a *= factor;
            }
            let plan = Plan::fit(&samples, &c, &base, &cancel).unwrap();
            let r = &plan.reports[0];
            let mean = r.signed_before.iter().flatten().sum::<f64>() / r.signed_before.len() as f64;
            assert_eq!(mean > 0.0, factor < 1.0, "factor={factor}, signed={mean}");
            assert!(r.accepted_iterations > 0);
            assert!(
                r.residual_rms_after < 0.15 * r.residual_rms_before,
                "factor={factor}, before={}, after={}",
                r.residual_rms_before,
                r.residual_rms_after
            );
        }
    }
    #[test]
    fn disagreements_between_row_sets_do_not_drive_refinement() {
        let (c, mut samples, _) = sample();
        let cancel = AtomicBool::new(false);
        let base = model::analyze(&samples, &c.options, &cancel).unwrap();
        // Deliberately incompatible scene/phase on validation rows.
        for (i, v) in samples.held[0].iter_mut().enumerate() {
            *v = 0.06 * (-0.08 * (TAU * (i % c.width) as f64 / 37.3 + 0.4).cos()).exp();
        }
        let plan = Plan::fit(&samples, &c, &base, &cancel).unwrap();
        assert_eq!(plan.reports[0].accepted_iterations, 0);
        assert_eq!(plan.fields[0].amplitudes, base.channels[0].amplitude);
    }
    #[test]
    fn weak_initial_confidence_does_not_lock_in_overcorrection() {
        let (c, samples, _) = sample();
        let cancel = AtomicBool::new(false);
        let mut base = model::analyze(&samples, &c.options, &cancel).unwrap();
        for a in base.channels[0].amplitude.iter_mut().flatten() {
            *a *= 1.7;
        }
        for v in base.channels[0].confidence.iter_mut().flatten() {
            *v = 0.1;
        }
        let plan = Plan::fit(&samples, &c, &base, &cancel).unwrap();
        let r = &plan.reports[0];
        assert!(r.accepted_iterations > 0);
        assert!(r.residual_rms_after < 0.15 * r.residual_rms_before);
        assert!(
            r.refined_amplitudes
                .iter()
                .flatten()
                .zip(base.channels[0].amplitude.iter().flatten())
                .all(|(a, b)| a <= b)
        );
    }
    #[test]
    fn global_improvement_cannot_hide_a_new_local_phase_inversion() {
        let r = |parallel| {
            Some(Residual {
                parallel,
                quadrature: 0.0,
            })
        };
        let before = vec![vec![r(0.1), r(0.001)]];
        let after = vec![vec![r(0.01), r(-0.01)]];
        assert!(score(&after) < score(&before));
        assert!(!inversion_safe(&before, &after, &[vec![0.1, 0.1]]));
        assert!(inversion_safe(
            &after,
            &[vec![r(0.005), r(-0.002)]],
            &[vec![0.1, 0.1]]
        ));
    }
    #[test]
    fn one_layer_predicts_blending_and_keeps_thin_areas_neutral() {
        let (c, samples, mut raw) = sample();
        let cancel = AtomicBool::new(false);
        let base = model::analyze(&samples, &c.options, &cancel).unwrap();
        let plan = Plan::fit(&samples, &c, &base, &cancel).unwrap();
        let mut analysis = Analysis::new(c, base);
        analysis.residual = Some(plan);
        raw[..384].fill(60000);
        let (layer, reference) = analysis.render_adaptive(&raw, 0, &cancel).unwrap();
        assert!(layer.mask[..384].iter().all(|&v| v == 0));
        for i in 0..raw.len() {
            let base = quantize(raw[i] as f64 / 65535.0) as f64 / 32768.0;
            let expected = quantize(
                base + layer.mask[i] as f64 / 32768.0
                    * ((base + 2.0 * layer.pixels[i] as f64 / 32768.0 - 1.0).clamp(0.0, 1.0)
                        - base),
            );
            assert_eq!(expected, quantize(reference[i] as f64 / 65535.0));
        }
        analysis.config.options.strength = 0.0;
        assert!(
            analysis
                .render_adaptive(&raw, 0, &cancel)
                .unwrap()
                .0
                .pixels
                .iter()
                .all(|&v| v == 16384)
        );
        assert!(
            analysis
                .render_adaptive(&raw, 0, &AtomicBool::new(true))
                .is_err()
        );
    }
    #[test]
    fn rgb_and_parallel_render_match_and_masks_are_darkness_only() {
        let (mut c, samples, raw) = sample();
        let cancel = AtomicBool::new(false);
        c.channels = 3;
        let samples = model::Samples {
            channels: 3,
            training: vec![samples.training[0].clone(); 3],
            held: vec![samples.held[0].clone(); 3],
            ..samples
        };
        let base = model::analyze(&samples, &c.options, &cancel).unwrap();
        let plan = Plan::fit(&samples, &c, &base, &cancel).unwrap();
        let mut analysis = Analysis::new(c, base);
        analysis.residual = Some(plan);
        // Widen only the render frame to exercise multiple workers; the fitted
        // fields repeat their edge amplitude but keep full-width carriers.
        analysis.config.height = 256;
        let raw: Vec<_> = raw
            .iter()
            .cycle()
            .take(384 * 256)
            .flat_map(|&v| [v, v, v])
            .collect();
        let (serial, reference) = analysis.render_adaptive(&raw, 0, &cancel).unwrap();
        let (parallel, predicted) = analysis.render_adaptive_parallel(&raw, 0, &cancel).unwrap();
        assert_eq!(serial.pixels, parallel.pixels);
        assert_eq!(serial.mask, parallel.mask);
        assert_eq!(reference, predicted);
        assert!(serial.mask.iter().all(|&v| v == 32768));
        for pixel in serial.pixels.as_chunks::<3>().0 {
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
        }
    }
}
