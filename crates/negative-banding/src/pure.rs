// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure carriers and independent spatial/density masks. No source pixels or
//! already-quantized correction deltas enter the carrier or spatial mask.
use super::{Analysis, Config, Error, Result, check_cancel, model, quantize};
use serde::Serialize;
use std::{f64::consts::TAU, sync::atomic::AtomicBool};

#[derive(Serialize)]
pub struct Component {
    pub index: usize,
    pub name: String,
    pub channel: usize,
    pub period_px: f64,
    pub frequency: f64,
    pub phase: f64,
    pub peak_log_amplitude: f64,
    pub carrier_gain: f64,
    #[serde(skip)]
    #[cfg(test)]
    row: Vec<u16>,
    #[serde(skip)]
    y: Vec<f64>,
    #[serde(skip)]
    spatial: Vec<f64>,
    #[serde(skip)]
    gray_density: Vec<u16>,
}
pub struct Plan {
    pub components: Vec<Component>,
}
#[cfg(test)]
pub struct PureTile {
    pub pixels: Vec<u16>,
    pub spatial: Vec<u16>,
    pub density: Vec<u16>,
}

fn bracket(centers: &[f64], value: f64) -> (usize, usize, f64) {
    if value <= centers[0] {
        return (0, 0, 0.0);
    }
    let last = centers.len() - 1;
    if value >= centers[last] {
        return (last, last, 0.0);
    }
    let hi = centers.partition_point(|&v| v <= value);
    let lo = hi - 1;
    (lo, hi, (value - centers[lo]) / (centers[hi] - centers[lo]))
}
impl Plan {
    pub fn new(config: &Config, model: &model::Model) -> Self {
        Self::with_refinement(config, model, None)
    }
    pub(super) fn with_refinement(
        config: &Config,
        model: &model::Model,
        refinement: Option<&super::residual::Plan>,
    ) -> Self {
        let mut components = Vec::new();
        // A gentler carrier makes the combined mask easier to inspect. Preserve
        // enough range for the density term even with unusually high cutoffs.
        // RGB uses max-channel brightness, so the same bound covers it too.
        let density_bound = (0..=65535u16)
            .map(|value| {
                let raw = value as f64 / 65535.0;
                raw * model::dark_weight(raw, config.options.dark_full, config.options.dark_off)
                    * config.options.strength
            })
            .fold(0.0, f64::max);
        for (channel, field) in model.channels.iter().enumerate() {
            let amplitudes = refinement
                .and_then(|plan| plan.reports.iter().find(|r| r.channel == channel))
                .map_or(&field.amplitude, |report| &report.refined_amplitudes);
            for (k, frequency) in field.frequencies.iter().enumerate() {
                let peak = amplitudes[k].iter().copied().fold(0.0, f64::max);
                if peak <= 0.0 {
                    continue;
                }
                let gain = (config.carrier_boost.max(density_bound) * peak).min(0.98);
                #[cfg(test)]
                let row = (0..config.width)
                    .map(|x| {
                        quantize(
                            0.5 - 0.5
                                * gain
                                * (TAU * frequency.frequency * x as f64 + frequency.phase).cos(),
                        )
                    })
                    .collect();
                let mut spatial = Vec::with_capacity(config.width * field.y.len());
                for values in amplitudes[k].chunks_exact(field.x.len()) {
                    for x in 0..config.width {
                        let (lo, hi, f) = bracket(&field.x, x as f64);
                        spatial.push(
                            ((values[lo] + f * (values[hi] - values[lo])) / peak).clamp(0.0, 1.0),
                        );
                    }
                }
                let channel_name = if config.channels == 1 {
                    "Gray"
                } else {
                    ["Red", "Green", "Blue"][channel]
                };
                let gray_density = if config.channels == 1 {
                    (0..=65535u16)
                        .map(|value| {
                            let raw = value as f64 / 65535.0;
                            let dark = model::dark_weight(
                                raw,
                                config.options.dark_full,
                                config.options.dark_off,
                            );
                            quantize(raw * dark * config.options.strength * peak / gain)
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                components.push(Component {
                    index: components.len(),
                    name: format!("{channel_name} / {:.2} px wave", frequency.period_px),
                    channel,
                    period_px: frequency.period_px,
                    frequency: frequency.frequency,
                    phase: frequency.phase,
                    peak_log_amplitude: peak,
                    carrier_gain: gain,
                    #[cfg(test)]
                    row,
                    y: field.y.clone(),
                    spatial,
                    gray_density,
                });
            }
        }
        Self { components }
    }
}
impl Component {
    fn spatial_row(&self, width: usize, y: usize, out: &mut [u16]) {
        let (lo, hi, f) = bracket(&self.y, y as f64);
        for (x, value) in out.iter_mut().enumerate() {
            let a = self.spatial[lo * width + x];
            let b = self.spatial[hi * width + x];
            *value = quantize(a + (b - a) * f);
        }
    }
    fn density(&self, config: &Config, raw: &[u16]) -> u16 {
        if config.channels == 1 {
            return self.gray_density[raw[0] as usize];
        }
        let brightness = *raw.iter().max().unwrap() as f64 / 65535.0;
        let darkness = model::dark_weight(
            brightness,
            config.options.dark_full,
            config.options.dark_off,
        );
        quantize(
            raw[self.channel] as f64 / 65535.0
                * darkness
                * config.options.strength
                * self.peak_log_amplitude
                / self.carrier_gain,
        )
    }
}
impl Analysis {
    /// A pure cosine and one combined mask per component, with the SAME target
    /// as the multi-group pure stack. The mask accounts for the actual clipped
    /// unmasked Linear Light blend; source detail never enters carrier pixels.
    pub fn render_compact(
        &self,
        index: Option<usize>,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<(Option<super::Tile>, Vec<u16>)> {
        let rows = self.pure_bounds(source, top)?;
        if index.is_some_and(|i| i >= self.pure.components.len()) {
            return Err(Error::Invalid("Unknown pure component".into()));
        }
        let c = &self.config;
        let mut output = index.map(|_| super::Tile {
            pixels: vec![16384; source.len()],
            mask: vec![0; source.len() / c.channels],
        });
        let mut result: Vec<_> = source
            .iter()
            .map(|&v| quantize(v as f64 / 65535.0))
            .collect();
        let mut spatial = vec![0; c.width];
        // Estimate band-free density without applying the user's strength or
        // darkness gate. All components are removed before deriving any mask.
        let mut clean = vec![0.0f64; source.len()];
        let mut signal = vec![0.0; c.width];
        for y in 0..rows {
            check_cancel(cancel)?;
            for channel in 0..c.channels {
                self.correction_region_row(channel, 0, top + y, &mut signal)?;
                for (x, &band) in signal.iter().enumerate() {
                    let i = (y * c.width + x) * c.channels + channel;
                    clean[i] = (source[i] as f64 / 65535.0 * (1.0 - band)).clamp(0.0, 1.0);
                }
            }
        }
        for (k, component) in self.pure.components.iter().enumerate() {
            if index.is_some_and(|i| k > i) {
                break;
            }
            let band_bound: f64 = self
                .pure
                .components
                .iter()
                .map(|v| v.peak_log_amplitude)
                .sum();
            for row in 0..rows {
                check_cancel(cancel)?;
                component.spatial_row(c.width, top + row, &mut spatial);
                for (x, &applicability) in spatial.iter().enumerate() {
                    let p = row * c.width + x;
                    let i = p * c.channels + component.channel;
                    let density =
                        component.density(c, &source[p * c.channels..(p + 1) * c.channels]);
                    // Bake only the smooth fitted envelope into the sine. Raw
                    // image density stays in the separately editable mask.
                    let original_carrier_pixel = quantize(
                        0.5 - 0.5
                            * component.carrier_gain
                            * (applicability as f64 / 32768.0)
                            * (TAU * component.frequency * x as f64 + component.phase).cos(),
                    );
                    let carrier = original_carrier_pixel as f64 / 32768.0;
                    let base = result[i] as f64 / 32768.0;
                    let delta = 2.0 * carrier - 1.0;
                    let density_weight = density as f64 / 32768.0;
                    let target = (base + delta * density_weight).clamp(0.0, 1.0);
                    let clean_pixel = &clean[p * c.channels..(p + 1) * c.channels];
                    let brightness = clean_pixel.iter().copied().fold(0.0, f64::max);
                    let level = clean_pixel[component.channel];
                    let dark =
                        model::dark_weight(brightness, c.options.dark_full, c.options.dark_off);
                    let desired = level * dark * c.options.strength * component.peak_log_amplitude
                        / component.carrier_gain
                        / c.compact_opacity;
                    // Reserve enough mask range for BOTH wave phases using a
                    // global amplitude bound, never the pixel's sine phase.
                    // This removes the old periodic clipping stripes.
                    let low = level / (1.0 + band_bound);
                    let high = if band_bound < 1.0 {
                        level / (1.0 - band_bound)
                    } else {
                        1.0
                    };
                    let dark_bound = model::dark_weight(
                        brightness / (1.0 + band_bound),
                        c.options.dark_full,
                        c.options.dark_off,
                    );
                    let shift_bound =
                        high * c.options.strength * component.peak_log_amplitude * dark_bound;
                    let available = (low * (1.0 - c.options.strength * band_bound))
                        .min(1.0 - high * (1.0 + c.options.strength * band_bound));
                    let reserve = if shift_bound == 0.0 {
                        0.0
                    } else if available > 0.0 {
                        shift_bound / (c.compact_opacity * available)
                    } else {
                        1.0
                    };
                    // Round the safety floor upward so quantization cannot
                    // undermine the bound. No sine phase enters this bound.
                    let mask =
                        quantize(desired).max((reserve.clamp(0.0, 1.0) * 32768.0).ceil() as u16);
                    let weight = c.compact_opacity * mask as f64 / 32768.0;
                    let carrier_pixel = quantize(
                        0.5 + if weight > 0.0 {
                            0.5 * (target - base) / weight
                        } else {
                            0.0
                        },
                    );
                    let full = (base + 2.0 * carrier_pixel as f64 / 32768.0 - 1.0).clamp(0.0, 1.0);
                    let change = full - base;
                    result[i] = quantize(base + c.compact_opacity * mask as f64 / 32768.0 * change);
                    // Opacity limits the maximum reachable correction even
                    // with a white mask. Never silently weaken the fitted result.
                    if result[i].abs_diff(quantize(target)) > 1 {
                        return Err(Error::Invalid("The fitted correction needs more opacity in some pixels. Increase Initial layer opacity (up to 100%) and run again.".into()));
                    }
                    if index == Some(k) {
                        let tile = output.as_mut().unwrap();
                        tile.pixels[i] = carrier_pixel;
                        tile.mask[p] = mask;
                    }
                }
            }
        }
        Ok((
            output,
            result
                .into_iter()
                .map(|v| (v as f64 / 32768.0 * 65535.0).round_ties_even() as u16)
                .collect(),
        ))
    }
    pub fn render_compact_parallel(
        &self,
        index: Option<usize>,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<(Option<super::Tile>, Vec<u16>)> {
        let rows = self.pure_bounds(source, top)?;
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
            .min(rows);
        if workers < 2 || source.len() < 262_144 {
            return self.render_compact(index, source, top, cancel);
        }
        let row = self.config.width * self.config.channels;
        let step = rows.div_ceil(workers);
        let parts = std::thread::scope(|scope| {
            let handles: Vec<_> = source
                .chunks(step * row)
                .enumerate()
                .map(|(i, part)| {
                    scope.spawn(move || self.render_compact(index, part, top + i * step, cancel))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(Error::Invalid("Compact pure render worker failed".into()))
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let mut tile = index.map(|_| super::Tile {
            pixels: Vec::with_capacity(source.len()),
            mask: Vec::with_capacity(source.len() / self.config.channels),
        });
        let mut result = Vec::with_capacity(source.len());
        for (part, mut reference) in parts {
            if let (Some(target), Some(mut source)) = (&mut tile, part) {
                target.pixels.append(&mut source.pixels);
                target.mask.append(&mut source.mask);
            }
            result.append(&mut reference);
        }
        Ok((tile, result))
    }
    fn pure_bounds(&self, source: &[u16], top: usize) -> Result<usize> {
        let row = self.config.width * self.config.channels;
        if source.is_empty()
            || !source.len().is_multiple_of(row)
            || top
                .checked_add(source.len() / row)
                .is_none_or(|end| end > self.config.height)
        {
            return Err(Error::Invalid("Invalid pure-wave strip bounds".into()));
        }
        Ok(source.len() / row)
    }
    #[cfg(test)]
    pub fn render_pure(
        &self,
        index: usize,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<PureTile> {
        let rows = self.pure_bounds(source, top)?;
        let component = self
            .pure
            .components
            .get(index)
            .ok_or_else(|| Error::Invalid("Unknown pure-wave component".into()))?;
        let c = &self.config;
        let mut tile = PureTile {
            pixels: vec![16384; source.len()],
            spatial: vec![0; c.width * rows],
            density: Vec::with_capacity(c.width * rows),
        };
        for row in 0..rows {
            check_cancel(cancel)?;
            component.spatial_row(
                c.width,
                top + row,
                &mut tile.spatial[row * c.width..(row + 1) * c.width],
            );
            for x in 0..c.width {
                let p = row * c.width + x;
                tile.pixels[p * c.channels + component.channel] = component.row[x];
                tile.density
                    .push(component.density(c, &source[p * c.channels..(p + 1) * c.channels]));
            }
        }
        Ok(tile)
    }

    #[cfg(test)]
    pub fn render_pure_parallel(
        &self,
        index: usize,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<PureTile> {
        let rows = self.pure_bounds(source, top)?;
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
            .min(rows);
        if workers < 2 || source.len() < 262_144 {
            return self.render_pure(index, source, top, cancel);
        }
        let step = rows.div_ceil(workers);
        let row_samples = self.config.width * self.config.channels;
        let parts = std::thread::scope(|scope| {
            let jobs: Vec<_> = source
                .chunks(step * row_samples)
                .enumerate()
                .map(|(i, part)| {
                    scope.spawn(move || self.render_pure(index, part, top + i * step, cancel))
                })
                .collect();
            jobs.into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(Error::Invalid("Pure render worker failed".into())))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let mut out = PureTile {
            pixels: Vec::with_capacity(source.len()),
            spatial: Vec::with_capacity(source.len() / self.config.channels),
            density: Vec::with_capacity(source.len() / self.config.channels),
        };
        for mut part in parts {
            out.pixels.append(&mut part.pixels);
            out.spatial.append(&mut part.spatial);
            out.density.append(&mut part.density);
        }
        Ok(out)
    }
    /// Predict the actual encoded pure-wave group stack, not the exponential
    /// epscan target. Keep these two references distinct in reports and UI.
    #[cfg(test)]
    pub fn pure_reference(
        &self,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u16>> {
        let rows = self.pure_bounds(source, top)?;
        let c = &self.config;
        let mut result: Vec<u16> = source
            .iter()
            .map(|&v| quantize(v as f64 / 65535.0))
            .collect();
        let mut spatial = vec![0; c.width];
        for component in &self.pure.components {
            for row in 0..rows {
                check_cancel(cancel)?;
                component.spatial_row(c.width, top + row, &mut spatial);
                for (x, &applicability) in spatial.iter().enumerate() {
                    let p = row * c.width + x;
                    let density =
                        component.density(c, &source[p * c.channels..(p + 1) * c.channels]);
                    // Both masks act on the carrier over neutral gray BEFORE
                    // the enclosing Linear Light group touches the negative.
                    let carrier = component.row[x] as f64 / 32768.0;
                    let group = quantize(
                        0.5 + (carrier - 0.5)
                            * (applicability as f64 / 32768.0)
                            * (density as f64 / 32768.0),
                    );
                    let i = p * c.channels + component.channel;
                    result[i] =
                        quantize(result[i] as f64 / 32768.0 + 2.0 * group as f64 / 32768.0 - 1.0);
                }
            }
        }
        Ok(result
            .into_iter()
            .map(|v| (v as f64 / 32768.0 * 65535.0).round_ties_even() as u16)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer_tests::fixture;
    #[test]
    fn band_free_density_mask_does_not_follow_the_wave_phase() {
        let (mut analysis, raw) = fixture(1, 0.5, 1.0);
        analysis.config.compact_opacity = 168.0 / 255.0;
        let mut banded = vec![0; raw.len()];
        for y in 0..analysis.config.height {
            for x in 0..analysis.config.width {
                let signal = analysis.correction(0, x, y).unwrap();
                banded[y * analysis.config.width + x] =
                    (0.01 / (1.0 - signal) * 65535.0).round() as u16;
            }
        }
        let tile = analysis
            .render_compact(Some(0), &banded, 0, &AtomicBool::new(false))
            .unwrap()
            .0
            .unwrap();
        let lo = *tile.mask.iter().min().unwrap();
        let hi = *tile.mask.iter().max().unwrap();
        assert!(hi.abs_diff(lo) <= 2, "mask follows phase: {lo}..{hi}");
        assert!(lo > 0);
        assert!(tile.pixels.iter().any(|&v| v != 16384));
    }
    #[test]
    fn gentle_carrier_brightens_mask_without_changing_correction() {
        for channels in [1, 3] {
            let (mut strong, raw) = fixture(channels, 1.5, 1.0);
            let (mut gentle, _) = fixture(channels, 0.5, 1.0);
            strong.config.compact_opacity = 168.0 / 255.0;
            gentle.config.compact_opacity = 168.0 / 255.0;
            let cancel = AtomicBool::new(false);
            let (before, old_result) = strong.render_compact(Some(0), &raw, 0, &cancel).unwrap();
            let (after, new_result) = gentle.render_compact(Some(0), &raw, 0, &cancel).unwrap();
            let before = before.unwrap();
            let after = after.unwrap();
            let old_peak = before
                .pixels
                .iter()
                .map(|v| v.abs_diff(16384))
                .max()
                .unwrap();
            let new_peak = after
                .pixels
                .iter()
                .map(|v| v.abs_diff(16384))
                .max()
                .unwrap();
            assert!(new_peak * 2 < old_peak);
            let old_sum: u64 = before.mask.iter().map(|&v| u64::from(v)).sum();
            let new_sum: u64 = after.mask.iter().map(|&v| u64::from(v)).sum();
            assert!(new_sum > 2 * old_sum);
            for (&a, &b) in old_result.iter().zip(&new_result) {
                assert!(a.abs_diff(b) <= 8, "changed correction: {a} vs {b}");
            }
            // A higher darkness cutoff must not silently clamp the density
            // term when the user requests a carrier below its required range.
            gentle.config.options.dark_full = 0.9;
            gentle.config.options.dark_off = 1.0;
            gentle.pure = Plan::new(&gentle.config, &gentle.model);
            for component in &gentle.pure.components {
                assert!(component.carrier_gain >= 0.9 * component.peak_log_amplitude);
            }
        }
    }
    #[test]
    fn initial_opacity_preserves_target_and_keeps_carrier_pure() {
        for channels in [1, 3] {
            let (mut analysis, raw) = fixture(channels, 1.5, 0.8);
            let cancel = AtomicBool::new(false);
            let before = analysis.render_compact(None, &raw, 0, &cancel).unwrap().1;
            let original = analysis
                .render_compact(Some(0), &raw, 0, &cancel)
                .unwrap()
                .0
                .unwrap();
            analysis.config.compact_opacity = 168.0 / 255.0;
            let after = analysis.render_compact(None, &raw, 0, &cancel).unwrap().1;
            let tile = analysis
                .render_compact(Some(0), &raw, 0, &cancel)
                .unwrap()
                .0
                .unwrap();
            for (&a, &b) in original.pixels.iter().zip(&tile.pixels) {
                assert!(a.abs_diff(b) <= 2);
            }
            assert!(tile.mask.iter().zip(&original.mask).any(|(&a, &b)| a > b));
            for (&a, &b) in before.iter().zip(&after) {
                assert!(
                    quantize(a as f64 / 65535.0).abs_diff(quantize(b as f64 / 65535.0))
                        <= analysis.pure.components.len() as u16
                );
            }
            let mut stronger = false;
            for (i, &raw) in raw.iter().enumerate() {
                let base = quantize(raw as f64 / 65535.0) as f64 / 32768.0;
                let change =
                    (base + 2.0 * tile.pixels[i] as f64 / 32768.0 - 1.0).clamp(0.0, 1.0) - base;
                let applied = tile.mask[i / channels] as f64 / 32768.0 * change;
                stronger |= (quantize(base + applied) as i32 - quantize(base) as i32).abs()
                    > (quantize(base + analysis.config.compact_opacity * applied) as i32
                        - quantize(base) as i32)
                        .abs();
            }
            assert!(
                stronger,
                "100% opacity must leave room above the fitted result"
            );
            analysis.config.compact_opacity = 0.01;
            assert!(
                analysis
                    .render_compact(None, &raw, 0, &cancel)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("Increase Initial layer opacity")
            );
        }
    }
    #[test]
    fn compact_carriers_stay_pure_and_masks_compensate_dense_clipping() {
        for channels in [1, 3] {
            let (analysis, raw) = fixture(channels, 1.5, 0.8);
            let dense = vec![327u16; raw.len()];
            let thin = vec![60000u16; raw.len()];
            let cancel = AtomicBool::new(false);
            for k in 0..analysis.pure.components.len() {
                let original = analysis
                    .render_compact(Some(k), &raw, 0, &cancel)
                    .unwrap()
                    .0
                    .unwrap();
                let altered = analysis
                    .render_compact(Some(k), &dense, 0, &cancel)
                    .unwrap()
                    .0
                    .unwrap();
                let protected = analysis
                    .render_compact(Some(k), &thin, 0, &cancel)
                    .unwrap()
                    .0
                    .unwrap();
                assert_ne!(original.pixels, altered.pixels);
                assert!(protected.pixels.iter().all(|&v| v == 16384));
                assert_ne!(original.mask, altered.mask);
                assert!(protected.mask.iter().all(|&v| v == 0));
            }
            let (_, expected) = analysis.render_compact(None, &dense, 0, &cancel).unwrap();
            let previous = analysis.pure_reference(&dense, 0, &cancel).unwrap();
            let mut blended: Vec<_> = dense
                .iter()
                .map(|&v| quantize(v as f64 / 65535.0))
                .collect();
            for k in 0..analysis.pure.components.len() {
                let tile = analysis
                    .render_compact(Some(k), &dense, 0, &cancel)
                    .unwrap()
                    .0
                    .unwrap();
                for (i, value) in blended.iter_mut().enumerate() {
                    let base = *value as f64 / 32768.0;
                    let full = (base + 2.0 * tile.pixels[i] as f64 / 32768.0 - 1.0).clamp(0.0, 1.0);
                    *value =
                        quantize(base + tile.mask[i / channels] as f64 / 32768.0 * (full - base));
                }
            }
            for i in 0..blended.len() {
                assert_eq!(blended[i], quantize(expected[i] as f64 / 65535.0));
                assert!(
                    blended[i].abs_diff(quantize(previous[i] as f64 / 65535.0))
                        <= analysis.pure.components.len() as u16
                );
            }
            assert!(analysis.render_compact(Some(99), &raw, 0, &cancel).is_err());
            assert!(
                analysis
                    .render_compact(None, &raw, 0, &AtomicBool::new(true))
                    .is_err()
            );
        }
    }
    #[test]
    fn compact_parallel_and_serial_are_identical() {
        let (mut analysis, raw) = fixture(3, 1.5, 0.8);
        analysis.config.height = 255;
        let raw: Vec<_> = raw.iter().copied().cycle().take(384 * 255 * 3).collect();
        for index in [None, Some(0)] {
            let a = analysis
                .render_compact(index, &raw, 0, &AtomicBool::new(false))
                .unwrap();
            let b = analysis
                .render_compact_parallel(index, &raw, 0, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(a.1, b.1);
            if let (Some(a), Some(b)) = (a.0, b.0) {
                assert_eq!(a.pixels, b.pixels);
                assert_eq!(a.mask, b.mask);
            }
        }
    }
    #[test]
    fn carriers_are_source_independent_and_identical_on_every_row() {
        for channels in [1, 3] {
            let (analysis, raw) = fixture(channels, 1.5, 0.8);
            for component in &analysis.pure.components {
                let a = analysis
                    .render_pure(component.index, &raw, 0, &AtomicBool::new(false))
                    .unwrap();
                let altered = vec![30000; raw.len()];
                let b = analysis
                    .render_pure(component.index, &altered, 0, &AtomicBool::new(false))
                    .unwrap();
                assert_eq!(a.pixels, b.pixels);
                assert_eq!(a.spatial, b.spatial);
                assert_ne!(a.density, b.density);
                for row in a.pixels.chunks_exact(384 * channels) {
                    assert_eq!(row, &a.pixels[..384 * channels]);
                }
                for x in 0..384 {
                    let expected = quantize(
                        0.5 - 0.5
                            * component.carrier_gain
                            * (TAU * component.frequency * x as f64 + component.phase).cos(),
                    );
                    assert_eq!(a.pixels[x * channels + component.channel], expected);
                }
                assert!(a.density[56 * 384..].iter().all(|&v| v == 0));
                let same = vec![10000; raw.len()];
                let density = analysis
                    .render_pure(component.index, &same, 0, &AtomicBool::new(false))
                    .unwrap()
                    .density;
                assert!(density.iter().all(|&v| v == density[0]));
            }
        }
    }
    #[test]
    fn pure_reference_matches_linearized_model_and_preserves_thin_areas() {
        for channels in [1, 3] {
            let (analysis, raw) = fixture(channels, 1.5, 0.8);
            let out = analysis
                .pure_reference(&raw, 0, &AtomicBool::new(false))
                .unwrap();
            for (i, &value) in raw.iter().enumerate() {
                let p = i / channels;
                let x = p % 384;
                let y = p / 384;
                let channel = i % channels;
                let brightness =
                    *raw[p * channels..(p + 1) * channels].iter().max().unwrap() as f64 / 65535.0;
                let dark = model::dark_weight(brightness, 0.1, 0.6);
                let expected = quantize(
                    value as f64 / 65535.0
                        * (1.0 - 0.8 * dark * analysis.model.correction(channel, x, y)),
                );
                assert!(quantize(out[i] as f64 / 65535.0).abs_diff(expected) <= 3);
                if dark == 0.0 {
                    assert_eq!(out[i], value);
                }
            }
            let slice = &raw[7 * 384 * channels..19 * 384 * channels];
            assert_eq!(
                analysis
                    .pure_reference(slice, 7, &AtomicBool::new(false))
                    .unwrap(),
                out[7 * 384 * channels..19 * 384 * channels]
            );
            assert!(
                analysis
                    .render_pure(999, &raw, 0, &AtomicBool::new(false))
                    .is_err()
            );
            assert!(
                analysis
                    .pure_reference(&raw, usize::MAX, &AtomicBool::new(false))
                    .is_err()
            );
            assert!(
                analysis
                    .pure_reference(&raw, 0, &AtomicBool::new(true))
                    .is_err()
            );
        }
        let (zero, raw) = fixture(1, 1.5, 0.0);
        let tile = zero
            .render_pure(0, &raw, 0, &AtomicBool::new(false))
            .unwrap();
        assert!(tile.density.iter().all(|&v| v == 0));
        assert_eq!(
            zero.pure_reference(&raw, 0, &AtomicBool::new(false))
                .unwrap(),
            raw
        );
    }

    #[test]
    fn parallel_pure_tiles_are_identical_to_serial() {
        for channels in [1, 3] {
            let (mut analysis, raw) = fixture(channels, 1.5, 0.8);
            let raw = raw.repeat(16);
            analysis.config.height = 1024;
            for component in &analysis.pure.components {
                let raw = &raw[7 * 384 * channels..];
                let serial = analysis
                    .render_pure(component.index, raw, 7, &AtomicBool::new(false))
                    .unwrap();
                let parallel = analysis
                    .render_pure_parallel(component.index, raw, 7, &AtomicBool::new(false))
                    .unwrap();
                assert_eq!(serial.pixels, parallel.pixels);
                assert_eq!(serial.spatial, parallel.spatial);
                assert_eq!(serial.density, parallel.density);
            }
        }
    }

    #[test]
    fn multiple_waves_add_to_the_linearized_correction() {
        use std::io::Write;
        let config = Config {
            compact_opacity: 1.0,
            residual_refine: false,
            width: 1024,
            height: 96,
            channels: 1,
            options: crate::BandingOptions {
                strength: 0.8,
                max_frequencies: 2,
                ..Default::default()
            },
            carrier_boost: 1.5,
        };
        let raw: Vec<u16> = (0..config.width * config.height)
            .map(|i| {
                let x = (i % config.width) as f64;
                let value = 0.12
                    * (0.025 * (TAU * x / 47.3 + 0.6).cos() + 0.016 * (TAU * x / 71.8 - 1.1).cos())
                        .exp();
                (quantize(value) as f64 / 32768.0 * 65535.0).round_ties_even() as u16
            })
            .collect();
        let mut file = tempfile::tempfile().unwrap();
        for value in &raw {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        let analysis = crate::analyze(&mut file, config, &AtomicBool::new(false)).unwrap();
        assert_eq!(analysis.pure.components.len(), 2);
        let output = analysis
            .pure_reference(&raw, 0, &AtomicBool::new(false))
            .unwrap();
        for (i, &sample) in raw.iter().enumerate() {
            let value = sample as f64 / 65535.0;
            let dark = model::dark_weight(value, 0.1, 0.6);
            let expected = quantize(
                value * (1.0 - 0.8 * dark * analysis.model.correction(0, i % 1024, i / 1024)),
            );
            assert!(quantize(output[i] as f64 / 65535.0).abs_diff(expected) <= 6);
        }
    }
}
