# Residual fitting for band correction

Version 0.12 uses an estimated debanded-density mask and puts image-dependent blend compensation in the correction layer. See [layer and mask equations](compact-pure-waves.md). Refinement now backs off inverse-phase residuals more readily and checks each measurable held node before accepting a step.

## Searching for the phase balance

For a fixed frequency and phase, the residual has a signed component along the
original wave. A positive coefficient means more correction is needed; a negative
coefficient means the correction has introduced the inverse wave and should be
reduced. A second, quadrature coefficient measures the out-of-phase component.

The implementation starts from the existing robust amplitude maps. It averages
sampled vertical rows into overlapping profiles, with the existing spatial grid
and fitting windows. Each local fit includes a quadratic image baseline and the
in-phase/quadrature responses of every detected frequency jointly. A Hann window
and three robust reweighting passes reduce the influence of edges and outliers.

The response basis includes original intensity and the darkness gate: it measures
what changing the applied correction would actually do, rather than fitting a
bare waveform and subsequently attenuating it with an unaccounted mask.

Up to eight refinement iterations:

1. Render the current interpolated correction on the training and held rows.
2. Estimate the signed remaining amplitude at each grid point.
3. Propose 95% of the smaller agreed residual when reducing amplitude, or 50% when increasing it.
4. Reject proposals with sign disagreement, inconsistent amplitude, substantial
   quadrature signal or little darkness support. Agreed reductions do not require initial detector confidence; increases still require 0.25. Reductions allow up to 75% row-set disagreement and quadrature up to the initial amplitude; increases retain the stricter limits.
5. Keep amplitudes nonnegative and bounded by both 2.5 times their initial value
   and the detector's existing global amplitude cap.
6. Re-render the entire candidate map on the held rows. Accept the iteration only
   if combined RMS decreases and no measurable held node develops or worsens an inverse-phase residual beyond 1% of its initial amplitude (minimum tolerance 1e-7). Try five successively halved mixed steps, then five reduction-only steps before abandoning the update.

Held rows participate in acceptance, so this is validation-guided tuning, not an
independent estimate of generalization error. Scene detail at the same frequency
and phase remains inherently ambiguous. The method aims toward a supported null;
it cannot guarantee a zero residual everywhere and does not force weak regions.
Nodes without adequate agreement retain their current correction. Lower detector confidence no longer blocks a reduction when both sampled row sets support it.

The search fits at **100% strength**. Default strength applies that result,
with masks normalized for the layer's actual starting opacity (66% by default).
See [combined masks](compact-pure-waves.md) for clipping and quantization details.
Hide an earlier correction before fitting the original negative again.

## Reports and verification

`residual_refinement` reports each fitted channel's accepted iteration count,
node update count (`adjusted_nodes`, including repeated updates), grid positions,
refined amplitude planes, signed residuals, and combined RMS before/after. Signed
arrays are frequency-major, then row-major on the amplitude grid. Null entries
mean insufficient support. These metrics describe the unquantized fit at 100%,
not necessarily a lower-strength output. The original detector diagnostics still
describe the exponential correction; they are not the residual-fitting metrics.

Automated checks cover deliberate under- and over-application, disagreement
between sampled row sets, darkness protection, zero strength, cancellation,
RGB channel consistency, serial/parallel identity, native tile transport, and
the one-layer/one-mask Photoshop workflow. Host integration is still checked
when the user runs the plugin; mocks cannot reproduce every Photoshop behavior.
