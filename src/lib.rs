// SPDX-License-Identifier: MIT OR Apache-2.0
//! Photoshop transport; numerical work lives in this repository's negative-banding crate.
pub use negative_banding::*;
pub mod native;

#[cfg(test)]
fn quantize(value: f64) -> u16 {
    (value.clamp(0.0, 1.0) * 32768.0).round_ties_even() as u16
}
