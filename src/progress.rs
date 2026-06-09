//! Progress bar written to stderr, built on [`indicatif`] so it survives
//! terminal resizes / tmux pane switches (it truncates to the terminal width,
//! clears to end of line, and redraws on resize instead of blindly overwriting
//! a single `\r` line).
//!
//! Columns: `parsed | kept | % of total | last kept | parsed/s | elapsed`,
//! where `parsed` is the total number of entities processed, `kept` is how many
//! passed the filter, and `parsed/s` is the average processing throughput.

use std::time::{Duration, Instant};

use indicatif::{ProgressBar as Spinner, ProgressDrawTarget, ProgressStyle};

fn header() -> String {
    format!(
        "{:>10} | {:>10} | {:>10} | {:>11} | {:>12} | {:>12}",
        "parsed", "kept", "% of total", "last kept", "parsed/s", "elapsed time",
    )
}

pub struct ProgressBar {
    spinner: Spinner,
    start: Instant,
    total: u64,
    kept: u64,
    last_entity_id: String,
}

impl ProgressBar {
    pub fn new() -> ProgressBar {
        // The header is static; print it once above the live line.
        eprintln!("{}", header());

        // A spinner with no length; the message holds the whole formatted line.
        // indicatif redraws at a few Hz and handles width/clearing/resize.
        let spinner = Spinner::new_spinner();
        spinner.set_draw_target(ProgressDrawTarget::stderr_with_hz(4));
        spinner.set_style(ProgressStyle::with_template("{msg}").unwrap());

        let mut bar = ProgressBar {
            spinner,
            start: Instant::now(),
            total: 0,
            kept: 0,
            last_entity_id: String::new(),
        };
        bar.refresh();
        bar
    }

    pub fn before_filter(&mut self) {
        self.total += 1;
    }

    pub fn after_filter(&mut self, entity_id: &str) {
        self.kept += 1;
        self.last_entity_id = entity_id.to_string();
        self.refresh();
    }

    pub fn after_negative_filter(&mut self) {
        self.refresh();
    }

    /// Aggregate counts for a whole processed block (used by the parallel path).
    pub fn add_block(&mut self, total_delta: u64, kept_delta: u64, last_id: Option<&str>) {
        self.total += total_delta;
        self.kept += kept_delta;
        if let Some(id) = last_id {
            self.last_entity_id = id.to_string();
        }
        self.refresh();
    }

    /// Render the final state and leave the line on screen.
    pub fn finish(&mut self) {
        self.refresh();
        self.spinner.finish();
    }

    fn refresh(&mut self) {
        let elapsed = self.start.elapsed();
        let rate = if self.total > 0 {
            round3(100.0 * self.kept as f64 / self.total as f64)
        } else {
            0.0
        };
        let secs = elapsed.as_secs_f64();
        let per_sec = if secs > 0.0 {
            (self.total as f64 / secs).round() as u64
        } else {
            0
        };

        let line = format!(
            "{:>10} | {:>10} | {:>9}% | {:>11} | {:>12} | {:>12}",
            self.total,
            self.kept,
            fmt_num(rate),
            self.last_entity_id,
            format!("{per_sec}/s"),
            format_elapsed(elapsed),
        );
        self.spinner.set_message(line);
    }
}

fn round3(n: f64) -> f64 {
    (1000.0 * n).round() / 1000.0
}

/// Format a number the way JS would (integers without a trailing `.0`).
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        // Trim trailing zeros for a JS-like representation.
        let s = format!("{:.3}", n);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

fn format_elapsed(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
