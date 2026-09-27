// SPDX-License-Identifier: MIT OR Apache-2.0
//! The epscan detector, with a Photoshop layer/mask renderer.
// The vendored detector retains helpers used by numerical oracle tests.
#[allow(dead_code)]
mod model;
mod pure;
mod residual;
pub mod error {
    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("{0}")]
        Invalid(String),
        #[error("Cancelled")]
        Cancelled,
        #[error(transparent)]
        Io(#[from] std::io::Error),
    }
    pub type Result<T> = std::result::Result<T, Error>;
}
pub use error::{Error, Result};
pub use model::{Samples, dark_weight};
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BandingOptions {
    pub strength: f64,
    pub dark_full: f64,
    pub dark_off: f64,
    pub max_frequencies: usize,
    pub min_period: f64,
    pub max_period: Option<f64>,
    pub window_cycles: f64,
    pub grid_y: usize,
    pub detection_roi: Option<[u32; 4]>,
    // Kept for source-compatible, byte-identical vendoring of epscan's tests.
    pub save_raw: bool,
    pub save_signal: bool,
}
impl Default for BandingOptions {
    fn default() -> Self {
        Self {
            strength: 1.0,
            dark_full: 0.1,
            dark_off: 0.6,
            max_frequencies: 3,
            min_period: 8.0,
            max_period: None,
            window_cycles: 4.0,
            grid_y: 12,
            detection_roi: None,
            save_raw: false,
            save_signal: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "full_opacity")]
    pub compact_opacity: f64,
    // Test-only baseline for numerical comparisons; production always refines.
    #[cfg(test)]
    #[serde(default)]
    pub residual_refine: bool,
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    #[serde(default)]
    pub options: BandingOptions,
    #[serde(default = "boost_default")]
    pub carrier_boost: f64,
}
fn boost_default() -> f64 {
    0.5
}
fn full_opacity() -> f64 {
    1.0
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        let o = &self.options;
        if !(41..=100_000).contains(&self.width)
            || !self.compact_opacity.is_finite()
            || !(0.0 < self.compact_opacity && self.compact_opacity <= 1.0)
            || !(16..=100_000).contains(&self.height)
            || !matches!(self.channels, 1 | 3)
            || self.byte_len()? > 16 * 1024 * 1024 * 1024
            || !(0.5..=3.0).contains(&self.carrier_boost)
            || !o.strength.is_finite()
            || !(0.0..=1.0).contains(&o.strength)
            || !o.dark_full.is_finite()
            || !o.dark_off.is_finite()
            || !(0.0 <= o.dark_full && o.dark_full < o.dark_off && o.dark_off <= 1.0)
            || !(1..=8).contains(&o.max_frequencies)
            || !(2..=128).contains(&o.grid_y)
            || !o.window_cycles.is_finite()
            || o.window_cycles < 3.0
            || !o.min_period.is_finite()
            || o.min_period < 2.0
        {
            return Err(Error::Invalid(
                "Invalid image dimensions or correction settings".into(),
            ));
        }
        let [x0, x1, y0, y1] =
            o.detection_roi
                .unwrap_or([0, self.width as u32, 0, self.height as u32]);
        let maximum = o.max_period.unwrap_or((x1.saturating_sub(x0)) as f64 / 5.0);
        if x0 >= x1
            || y0 >= y1
            || x1 as usize > self.width
            || y1 as usize > self.height
            || !maximum.is_finite()
            || maximum <= o.min_period
            || maximum > (x1 - x0) as f64 / 3.0
        {
            return Err(Error::Invalid(
                "Invalid detection ROI or period bounds".into(),
            ));
        }
        let (rows, held) = sample_rows(self.height);
        if [&rows, &held].iter().any(|ys| {
            ys.iter()
                .filter(|&&y| y >= y0 as usize && y < y1 as usize)
                .count()
                < 8
        }) {
            return Err(Error::Invalid(
                "ROI needs at least eight training and validation rows".into(),
            ));
        }
        Ok(())
    }
    pub fn byte_len(&self) -> Result<u64> {
        (self.width as u64)
            .checked_mul(self.height as u64)
            .and_then(|n| n.checked_mul(self.channels as u64))
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| Error::Invalid("Image size overflow".into()))
    }
    pub fn tile_height(&self) -> usize {
        (1_048_576 / (self.width * (self.channels + 1) * 2)).clamp(1, 64)
    }
}

pub fn sample_rows(height: usize) -> (Vec<usize>, Vec<usize>) {
    let stride = 2 * height.div_ceil(1536).max(1);
    (
        (0..height).step_by(stride).collect(),
        (stride / 2..height).step_by(stride).collect(),
    )
}
pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize)]
pub struct LayerSpec {
    pub name: String,
    pub blend_mode: &'static str,
}
pub struct Analysis {
    pub config: Config,
    #[cfg(test)]
    pub layers: Vec<LayerSpec>,
    pub model: model::Model,
    pure: pure::Plan,
    residual: Option<residual::Plan>,
}
pub struct Tile {
    pub pixels: Vec<u16>,
    pub mask: Vec<u16>,
}
#[cfg(test)]
pub struct Rendered {
    pub layers: Vec<Tile>,
    /// Full-range 16-bit output of the original epscan pixel kernel.
    pub reference: Vec<u16>,
}

pub fn analyze(file: &mut File, config: Config, cancel: &AtomicBool) -> Result<Analysis> {
    config.validate()?;
    if file.metadata()?.len() != config.byte_len()? {
        return Err(Error::Invalid("Incomplete source image".into()));
    }
    let (rows, held_rows) = sample_rows(config.height);
    let mut read = |ys: &[usize]| -> Result<Vec<Vec<f64>>> {
        let mut result = vec![Vec::with_capacity(ys.len() * config.width); config.channels];
        let mut packed = vec![0; config.width * config.channels * 2];
        for &y in ys {
            check_cancel(cancel)?;
            file.seek(SeekFrom::Start((y * packed.len()) as u64))?;
            file.read_exact(&mut packed)?;
            for pixel in packed.chunks_exact(2 * config.channels) {
                for (c, values) in result.iter_mut().enumerate() {
                    values.push(
                        u16::from_le_bytes([pixel[c * 2], pixel[c * 2 + 1]]) as f64 / 65535.0,
                    );
                }
            }
        }
        Ok(result)
    };
    let training = read(&rows)?;
    let held = read(&held_rows)?;
    let samples = model::Samples {
        width: config.width,
        height: config.height,
        channels: config.channels,
        max_value: 65535.0,
        rows,
        held_rows,
        training,
        held,
    };
    analyze_samples(samples, config, cancel)
}

pub fn analyze_samples(samples: Samples, config: Config, cancel: &AtomicBool) -> Result<Analysis> {
    config.validate()?;
    if samples.width != config.width
        || samples.height != config.height
        || samples.channels != config.channels
    {
        return Err(Error::Invalid(
            "Sample dimensions do not match configuration".into(),
        ));
    }
    let fitted = model::analyze(&samples, &config.options, cancel)?;
    #[cfg(not(test))]
    let residual = Some(residual::Plan::fit(&samples, &config, &fitted, cancel)?);
    #[cfg(test)]
    let residual = if config.residual_refine {
        Some(residual::Plan::fit(&samples, &config, &fitted, cancel)?)
    } else {
        None
    };
    let mut analysis = Analysis::new(config, fitted);
    analysis.pure =
        pure::Plan::with_refinement(&analysis.config, &analysis.model, residual.as_ref());
    analysis.residual = residual;
    Ok(analysis)
}

fn quantize(value: f64) -> u16 {
    (value.clamp(0.0, 1.0) * 32768.0).round_ties_even() as u16
}
impl Analysis {
    /// Refined floating-point signal in full-source coordinates. Cropped TIFF
    /// exports keep the same phase/amplitudes as the full acquisition.
    pub fn correction_region_row(
        &self,
        channel: usize,
        x: usize,
        y: usize,
        out: &mut [f64],
    ) -> Result<()> {
        if channel >= self.config.channels
            || y >= self.config.height
            || x.checked_add(out.len())
                .is_none_or(|v| v > self.config.width)
        {
            return Err(Error::Invalid(
                "Correction region outside source image".into(),
            ));
        }
        if let Some(refinement) = &self.residual {
            refinement.region_row(channel, x, y, out);
        } else {
            for (dx, value) in out.iter_mut().enumerate() {
                *value = self.model.correction(channel, x + dx, y);
            }
        }
        Ok(())
    }
    pub fn correction(&self, channel: usize, x: usize, y: usize) -> Result<f64> {
        let mut value = [0.0];
        self.correction_region_row(channel, x, y, &mut value)?;
        Ok(value[0])
    }
    fn new(config: Config, model: model::Model) -> Self {
        let pure = pure::Plan::new(&config, &model);
        #[cfg(test)]
        let layers = if model.has_components() {
            vec![LayerSpec {
                name: format!(
                    "Band correction / {:.0}% signal / {:.0}% applied",
                    config.carrier_boost * 100.0,
                    config.options.strength * 100.0
                ),
                blend_mode: "linearLight",
            }]
        } else {
            vec![]
        };
        Self {
            config,
            #[cfg(test)]
            layers,
            model,
            pure,
            residual: None,
        }
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"config":self.config,"diagnostics":self.model,"engine_version":VERSION,
            "pure_components":self.pure.components,
            "residual_refinement":self.residual.as_ref().map(|p|&p.reports),
            "residual_refinement_notes":"Fixed-phase residual fitting at 100% strength; disjoint held rows gate amplitude updates. Low-confidence regions retain initial estimates. User strength scales the result; darkness protection remains active.",
            "composition":"Linear Light correction pixels contain fitted waves and image-dependent compensation. Editable masks use estimated debanded density with phase-independent headroom; the fitted target is preserved at initial opacity.",
            "layer_sample_max":32768,"source_sample_max":65535,"tile_height":self.config.tile_height()})
    }
    // Numerical oracle only; not exported in the plugin library.
    #[cfg(test)]
    pub fn render(&self, source: &[u16], top: usize, cancel: &AtomicBool) -> Result<Rendered> {
        let c = &self.config;
        let row_samples = c.width * c.channels;
        if source.is_empty()
            || !source.len().is_multiple_of(row_samples)
            || top + source.len() / row_samples > c.height
        {
            return Err(Error::Invalid("Invalid source strip bounds".into()));
        }
        let count = source.len() / c.channels;
        let mut tiles: Vec<_> = self
            .layers
            .iter()
            .map(|_| Tile {
                pixels: vec![16384; source.len()],
                mask: vec![0; count],
            })
            .collect();
        let mut reference = Vec::with_capacity(source.len());
        let mut correction_rows = vec![vec![0.0; c.width]; c.channels];
        for p in 0..count {
            let x = p % c.width;
            let y = top + p / c.width;
            if x == 0 {
                check_cancel(cancel)?;
                for (channel, row) in correction_rows.iter_mut().enumerate() {
                    self.model.correction_row(channel, y, row);
                }
            }
            let raw = &source[p * c.channels..(p + 1) * c.channels];
            let brightness = *raw.iter().max().unwrap() as f64 / 65535.0;
            let dark = model::dark_weight(brightness, c.options.dark_full, c.options.dark_off);
            for (channel, &value) in raw.iter().enumerate() {
                reference.push(model::corrected_sample(
                    value,
                    65535,
                    correction_rows[channel][x],
                    dark,
                    c.options.strength,
                ));
            }
            if let Some(tile) = tiles.first_mut() {
                let mut base = [0.0; 3];
                let mut delta = [0.0; 3];
                let mut mask = dark * c.options.strength / c.carrier_boost;
                for channel in 0..c.channels {
                    base[channel] = quantize(raw[channel] as f64 / 65535.0) as f64 / 32768.0;
                    let target = quantize(reference[p * c.channels + channel] as f64 / 65535.0)
                        as f64
                        / 32768.0;
                    delta[channel] = target - base[channel];
                    // Keep the fully blended pixel in gamut BEFORE Photoshop applies opacity.
                    let room = if delta[channel] >= 0.0 {
                        1.0 - base[channel]
                    } else {
                        base[channel]
                    };
                    if room > 0.0 {
                        mask = mask.max(delta[channel].abs() / room);
                    }
                }
                let encoded_mask = (mask.clamp(0.0, 1.0) * 32768.0).ceil() as u16;
                tile.mask[p] = encoded_mask;
                let mask = encoded_mask as f64 / 32768.0;
                for channel in 0..c.channels {
                    let full_delta = if mask > 0.0 {
                        delta[channel] / mask
                    } else {
                        // Retain the unmasked fitted signal even in protected/thin areas.
                        base[channel] * (-c.carrier_boost * correction_rows[channel][x]).exp_m1()
                    };
                    tile.pixels[p * c.channels + channel] = quantize(0.5 + 0.5 * full_delta);
                }
            }
        }
        Ok(Rendered {
            layers: tiles,
            reference,
        })
    }

    /// Parallelize independent row ranges without changing the numerical kernel
    /// or per-pixel rounding. Limit concurrency to keep Photoshop responsive.
    #[cfg(test)]
    pub fn render_parallel(
        &self,
        source: &[u16],
        top: usize,
        cancel: &AtomicBool,
    ) -> Result<Rendered> {
        let row_samples = self.config.width * self.config.channels;
        if source.is_empty()
            || !source.len().is_multiple_of(row_samples)
            || top
                .checked_add(source.len() / row_samples)
                .is_none_or(|end| end > self.config.height)
        {
            return Err(Error::Invalid("Invalid source strip bounds".into()));
        }
        let rows = source.len() / row_samples;
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
            .min(rows);
        if workers < 2 || source.len() < 262_144 {
            return self.render(source, top, cancel);
        }
        let rows_per_worker = rows.div_ceil(workers);
        let parts = std::thread::scope(|scope| {
            let handles: Vec<_> = source
                .chunks(rows_per_worker * row_samples)
                .enumerate()
                .map(|(i, chunk)| {
                    scope.spawn(move || self.render(chunk, top + i * rows_per_worker, cancel))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(Error::Invalid("Render worker failed".into())))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let mut result = Rendered {
            layers: self
                .layers
                .iter()
                .map(|_| Tile {
                    pixels: Vec::with_capacity(source.len()),
                    mask: Vec::with_capacity(source.len() / self.config.channels),
                })
                .collect(),
            reference: Vec::with_capacity(source.len()),
        };
        for mut part in parts {
            result.reference.append(&mut part.reference);
            for (target, mut tile) in result.layers.iter_mut().zip(part.layers) {
                target.pixels.append(&mut tile.pixels);
                target.mask.append(&mut tile.mask);
            }
        }
        Ok(result)
    }
}

/// Direct TIFF kernel for the linear locally fitted wave correction. Quantize once at
/// the source bit depth instead of emulating Photoshop's 0..32768 layers.
pub fn linear_corrected_sample(
    raw: u16,
    maximum: u16,
    signal: f64,
    darkness: f64,
    strength: f64,
) -> u16 {
    (f64::from(raw) * (1.0 - strength * darkness * signal))
        .clamp(0.0, f64::from(maximum))
        .round_ties_even() as u16
}

#[cfg(test)]
mod layer_tests;
