//! Linear-predictive-coding analysis primitives (autocorrelation +
//! Levinson–Durbin), shared by the formant engine and the test/evidence
//! measurement helpers. All math runs in `f64`; inputs may be `f32`.

/// Biased autocorrelation `r[lag] = Σ x[i]·x[i+lag]` for `lag = 0..=order`.
pub fn autocorr(x: &[f32], order: usize) -> Vec<f64> {
    let mut r = vec![0.0f64; order + 1];
    let n = x.len();
    for lag in 0..=order {
        if lag >= n {
            break;
        }
        let mut acc = 0.0f64;
        for i in 0..n - lag {
            acc += f64::from(x[i]) * f64::from(x[i + lag]);
        }
        r[lag] = acc / f64::from(n as u32);
    }
    r
}

/// Levinson–Durbin reflection solve of `R·a = r`.
///
/// Returns `(a, error)` where `a` are the predictor coefficients
/// `a[1..=order]` (so `a[0]` is the implicit 1.0 and is *not* stored) and
/// `error` is the final residual energy (LPC gain² before gain matching).
pub fn levinson_durbin(r: &[f64], order: usize) -> (Vec<f64>, f64) {
    debug_assert!(r.len() > order, "autocorrelation shorter than order");
    let mut a = vec![0.0f64; order + 1];
    if r[0] <= 0.0 {
        return (a, 0.0);
    }

    let mut err = r[0];
    a[0] = 1.0; // kept implicit by callers; stored for clarity of the recursion
    let _ = a[0];

    let mut tmp = vec![0.0f64; order + 1];
    for k in 1..=order {
        let mut acc = r[k];
        for j in 1..k {
            acc -= a[j] * r[k - j];
        }
        let reflection = acc / err;
        // Unstable reflection coefficients would make the inverse filter
        // explode; clamp to the open interval as a numerical guardrail.
        let reflection = reflection.clamp(-0.999_999, 0.999_999);

        tmp[1..k].copy_from_slice(&a[1..k]);
        for j in 1..k {
            tmp[j] = a[j] - reflection * a[k - j];
        }
        tmp[k] = reflection;
        a[1..=k].copy_from_slice(&tmp[1..=k]);
        err *= 1.0 - reflection * reflection;
        if err <= 1e-12 {
            // Perfectly predictable signal: remaining orders stay zero.
            for v in &mut a[k + 1..] {
                *v = 0.0;
            }
            break;
        }
    }
    (a[1..=order].to_vec(), err)
}

/// Pre-emphasis filter `y[n] = x[n] − pre·x[n−1]` (in place semantics via
/// new vector; `pre` typically 0.97 for speech).
pub fn pre_emphasize(x: &[f32], pre: f32) -> Vec<f32> {
    let mut y = Vec::with_capacity(x.len());
    let mut prev = 0.0f32;
    for &s in x {
        y.push(s - pre * prev);
        prev = s;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levinson_recovers_single_pole() {
        // Generate y[n] = 0.9·y[n−1] + e[n] and check the estimated pole.
        let mut rng = crate::synth::Rng::new(11);
        let n = 8_000;
        let mut y = vec![0.0f32; n];
        for i in 1..n {
            y[i] = 0.9 * y[i - 1] + rng.next_bipolar() * 0.3;
        }
        let r = autocorr(&y, 4);
        let (a, _) = levinson_durbin(&r, 4);
        assert!((a[0] - 0.9).abs() < 0.05, "a1 = {}", a[0]);
    }

    #[test]
    fn levinson_handles_silence() {
        let r = autocorr(&[0.0f32; 64], 4);
        let (a, err) = levinson_durbin(&r, 4);
        assert!(a.iter().all(|&v| v == 0.0));
        assert_eq!(err, 0.0);
    }

    #[test]
    fn pre_emphasis_matches_definition() {
        let y = pre_emphasize(&[1.0, 0.5, 0.25], 0.97);
        assert_eq!(y[0], 1.0);
        assert!((y[1] - (0.5 - 0.97)).abs() < 1e-7);
        assert!((y[2] - (0.25 - 0.97 * 0.5)).abs() < 1e-7);
    }
}
