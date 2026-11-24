use crate::format::{format_duration, human_size};

#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
}

impl Progress {
    pub fn percent(&self) -> Option<f64> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        Some((self.downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }
}

pub fn parse_line(line: &str) -> Option<Progress> {
    let rest = line.trim().strip_prefix("PROGRESS ")?;
    let mut parts = rest.split_whitespace();

    let downloaded = parse_u64(parts.next()?)?;
    let total_bytes = parse_u64(parts.next().unwrap_or("NA"));
    let total_estimate = parse_u64(parts.next().unwrap_or("NA"));
    let speed = parse_f64(parts.next().unwrap_or("NA"));
    let eta = parse_u64(parts.next().unwrap_or("NA"));

    Some(Progress {
        downloaded,
        total: total_bytes.or(total_estimate),
        speed,
        eta,
    })
}

fn parse_u64(value: &str) -> Option<u64> {
    if is_missing(value) {
        None
    } else {
        value.parse().ok()
    }
}

fn parse_f64(value: &str) -> Option<f64> {
    if is_missing(value) {
        None
    } else {
        value.parse().ok()
    }
}

fn is_missing(value: &str) -> bool {
    matches!(value, "" | "NA" | "N/A" | "None" | "none" | "null")
}

pub fn render_bar(percent: f64, width: usize) -> String {
    let filled = ((percent.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn render_progress(progress: Progress, quality_label: &str) -> String {
    let percent = progress.percent();
    let bar = percent
        .map(|value| render_bar(value, 10))
        .unwrap_or_else(|| "░".repeat(10));
    let percent_text = percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "?".to_string());
    let total = progress
        .total
        .map(human_size)
        .unwrap_or_else(|| "?".to_string());
    let speed = progress
        .speed
        .map(|value| format!("{}/s", human_size(value.max(0.0) as u64)))
        .unwrap_or_else(|| "?".to_string());
    let eta = progress
        .eta
        .map(|value| format_duration(value as f64))
        .unwrap_or_else(|| "?".to_string());

    format!(
        "⬇️ Downloading {quality_label}...\n{bar} {percent_text}\n{} / {total} • {speed} • ETA {eta}",
        human_size(progress.downloaded)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_line() {
        let progress =
            parse_line("PROGRESS 123456 500000 480000 2048.5 42").expect("progress line");
        assert_eq!(progress.downloaded, 123456);
        assert_eq!(progress.total, Some(500000));
        assert_eq!(progress.speed, Some(2048.5));
        assert_eq!(progress.eta, Some(42));
        assert!(progress.percent().unwrap() > 24.0);

        let estimate =
            parse_line("PROGRESS 5000 NA 10000 2048.5 10").expect("estimate progress line");
        assert_eq!(estimate.total, Some(10000));
        assert_eq!(estimate.percent(), Some(50.0));
    }

    #[test]
    fn parses_missing_values() {
        let progress = parse_line("PROGRESS 1000 NA NA NA NA").expect("progress line");
        assert_eq!(progress.downloaded, 1000);
        assert_eq!(progress.total, None);
        assert_eq!(progress.speed, None);
        assert_eq!(progress.eta, None);
        assert_eq!(progress.percent(), None);
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse_line("[download] 10%").is_none());
        assert!(parse_line("PROGRESS NA 100 NA NA NA").is_none());
    }

    #[test]
    fn draws_bar() {
        assert_eq!(render_bar(0.0, 4), "░░░░");
        assert_eq!(render_bar(50.0, 4), "██░░");
        assert_eq!(render_bar(100.0, 4), "████");
    }
}
