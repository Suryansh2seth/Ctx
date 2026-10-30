//! Token estimation.
//!
//! The implemented mode is a pure byte heuristic: `tokens = ceil(bytes / ratio)`.
//! Source code tokenizes at roughly 3-4 bytes per token for common BPE
//! vocabularies, so the default ratio of 3.5 is deliberately a little
//! conservative. Every number produced here is an *estimate* and is labelled
//! `(est.)` wherever it is shown to a human.

use crate::ui::commas;

/// Suffix used whenever an estimate is reported to a human.
pub const EST: &str = "(est.)";

/// Annotation appended to a tree line for a file dropped by `--truncate`.
pub const OMIT_ANNOTATION: &str = "  [omitted: budget]";

/// `ceil(bytes / ratio)`.
///
/// `ratio` comes from `--ratio` and is validated to be finite and > 0 by the
/// CLI layer; the guard here keeps the function total for every input anyway.
#[inline]
pub fn estimate_bytes(bytes: u64, ratio: f64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    let r = if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        crate::cli::DEFAULT_RATIO
    };
    (bytes as f64 / r).ceil() as u64
}

/// Estimated tokens for a rendered string.
#[inline]
pub fn estimate_str(s: &str, ratio: f64) -> u64 {
    estimate_bytes(s.len() as u64, ratio)
}

/// Human-readable estimate that names the unit, e.g. `38,212 tokens (est.)`.
pub fn format_tokens(tokens: u64) -> String {
    format!("{} tokens {EST}", commas(tokens))
}

/// Bytes added to a tree line when a file is annotated as omitted.
#[inline]
pub fn annotation_bytes() -> u64 {
    OMIT_ANNOTATION.len() as u64
}

// ---------------------------------------------------------------------------
// `exact` feature.
//
// A real BPE tokenizer is deliberately NOT wired up yet. The feature gate, the
// call site and the fallback are real and tested, but the estimator itself is a
// stub that returns `None`, so every caller falls back to the byte heuristic.
// Enabling `exact` therefore changes nothing today except that a one-time
// notice is printed to stderr. See README ("Estimates, not exact").
// ---------------------------------------------------------------------------

/// Exact token count for a payload, when an exact tokenizer is available.
///
/// Stub: always `None`. This is the single place a BPE tokenizer would plug in,
/// which is why the call below is not feature-gated: the gate only controls the
/// notice, so the fallback path is exercised on every run and in every build.
pub fn exact_estimate(_payload: &str) -> Option<u64> {
    None
}

/// One-line stderr notice for `--features exact`; `None` when the feature is
/// off.
pub fn exact_notice() -> Option<&'static str> {
    #[cfg(feature = "exact")]
    let notice = Some(
        "built with --features exact, but the exact BPE tokenizer is a stub; using the byte heuristic",
    );
    #[cfg(not(feature = "exact"))]
    let notice = None;
    notice
}

/// Estimate the token count of a fully rendered payload.
pub fn estimate_payload(payload: &str, ratio: f64) -> u64 {
    if let Some(exact) = exact_estimate(payload) {
        return exact;
    }
    estimate_str(payload, ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_bytes_is_zero_tokens() {
        assert_eq!(estimate_bytes(0, 3.5), 0);
    }

    #[test]
    fn rounds_up() {
        // 7 / 3.5 == 2 exactly.
        assert_eq!(estimate_bytes(7, 3.5), 2);
        // 8 / 3.5 == 2.28..., any fraction rounds up.
        assert_eq!(estimate_bytes(8, 3.5), 3);
        // 1 byte is still 1 token, never 0.
        assert_eq!(estimate_bytes(1, 3.5), 1);
    }

    #[test]
    fn honours_ratio() {
        assert_eq!(estimate_bytes(8, 4.0), 2);
        assert_eq!(estimate_bytes(8, 1.0), 8);
        assert_eq!(estimate_bytes(100, 10.0), 10);
    }

    #[test]
    fn invalid_ratio_falls_back_to_default() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                estimate_bytes(7, bad),
                estimate_bytes(7, crate::cli::DEFAULT_RATIO),
                "ratio {bad} should fall back"
            );
        }
    }

    #[test]
    fn string_estimate_matches_byte_estimate() {
        let s = "fn main() {}\n";
        assert_eq!(estimate_str(s, 3.5), estimate_bytes(s.len() as u64, 3.5));
    }

    #[test]
    fn formatting_is_labelled_as_an_estimate() {
        assert_eq!(format_tokens(38_212), "38,212 tokens (est.)");
        assert_eq!(format_tokens(0), "0 tokens (est.)");
        assert_eq!(format_tokens(1_234_567), "1,234,567 tokens (est.)");
    }

    #[test]
    fn annotation_cost_is_the_annotation_length() {
        assert_eq!(annotation_bytes(), OMIT_ANNOTATION.len() as u64);
        assert_eq!(estimate_bytes(annotation_bytes(), 3.5), 6);
    }

    #[test]
    fn payload_estimate_uses_the_heuristic_when_exact_is_unavailable() {
        let payload = "<context>\n</context>\n";
        assert_eq!(estimate_payload(payload, 3.5), estimate_str(payload, 3.5));
    }

    #[test]
    fn exact_stub_reports_nothing() {
        assert!(exact_estimate("hello world").is_none());
        assert!(exact_estimate("").is_none());
    }

    #[test]
    fn exact_notice_is_only_emitted_with_the_feature() {
        if cfg!(feature = "exact") {
            assert!(exact_notice().is_some());
        } else {
            assert!(exact_notice().is_none());
        }
    }
}
