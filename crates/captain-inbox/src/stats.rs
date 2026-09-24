//! Small summary statistics for reports.

/// Nearest-rank percentile of `values`, `p` in `0..=100`. `None` for an empty slice.
pub fn percentile(values: &[i64], p: u8) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (usize::from(p.min(100)) * sorted.len())
        .div_ceil(100)
        .max(1);
    sorted.get(rank - 1).copied()
}

/// `part / whole`, or `None` when `whole` is zero.
#[expect(clippy::cast_precision_loss, reason = "counts stay far below 2^52")]
pub fn ratio(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

/// Arithmetic mean, or `None` for an empty slice.
#[expect(clippy::cast_precision_loss, reason = "counts stay far below 2^52")]
pub fn mean(values: &[i64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<i64>() as f64 / values.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let v = [5, 1, 4, 2, 3];
        assert_eq!(percentile(&v, 50), Some(3));
        assert_eq!(percentile(&v, 90), Some(5));
        assert_eq!(percentile(&v, 0), Some(1));
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn ratios_and_means() {
        assert_eq!(ratio(1, 4), Some(0.25));
        assert_eq!(ratio(1, 0), None);
        assert_eq!(mean(&[1, 2, 3]), Some(2.0));
    }
}
