//! Shared driver for the per-line "read → process → write" loop.
//!
//! The filter path (`main`) and the `build-graph` subcommand have identical
//! plumbing: decide the buffering mode and worker count, open the input, and —
//! on the sequential path — run a read loop that processes each line, accounts
//! for progress, and flushes per line when line-buffered. Centralising it here
//! keeps the two paths from drifting; each supplies only its own per-line
//! closure.

use std::io::{self, BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::process::LineOutcome;
use crate::progress::ProgressBar;

/// Resolve the buffering mode and worker count from the CLI flags.
///
/// Block-buffered favours throughput; line-buffered makes output appear
/// incrementally and implies the sequential path (1 worker) — the parallel
/// reader batches stdin into large blocks, which would defeat incremental
/// output. Line buffering auto-enables on a terminal; an explicit `--threads`
/// (or a non-terminal stdout) means block buffering.
pub fn dispatch(
    line_buffered_flag: bool,
    threads: Option<usize>,
    stdout_tty: bool,
) -> (bool, usize) {
    let line_buffered = if line_buffered_flag {
        true
    } else if threads.is_some() {
        false
    } else {
        stdout_tty
    };
    let workers = if line_buffered {
        1
    } else {
        threads
            .or_else(|| thread::available_parallelism().ok().map(|n| n.get()))
            .unwrap_or(1)
            .max(1)
    };
    (line_buffered, workers)
}

/// Open the input source: the named file, or stdin when no path is given.
pub fn open_input(path: Option<&str>) -> Result<Box<dyn BufRead>> {
    match path {
        Some(p) => {
            let file = std::fs::File::open(p).with_context(|| format!("cannot open input {p}"))?;
            Ok(Box::new(BufReader::with_capacity(1 << 20, file)))
        }
        None => Ok(Box::new(io::stdin().lock())),
    }
}

/// Apply one line's outcome to the sequential progress bar.
fn account(progress: &mut Option<ProgressBar>, outcome: &LineOutcome) {
    if let Some(p) = progress.as_mut() {
        if outcome.is_entity {
            p.before_filter();
        }
        if outcome.kept {
            p.after_filter(outcome.last_id.as_deref().unwrap_or(""));
        } else if outcome.is_entity {
            p.after_negative_filter();
        }
    }
}

/// Run the sequential read → process → write loop over `input`.
///
/// `process` handles one raw line, writing any output to the supplied writer and
/// returning what happened (for progress). A broken pipe downstream (e.g.
/// `| head`) is a clean stop, not an error.
pub fn run_sequential<F>(
    input: Option<String>,
    mut progress: Option<ProgressBar>,
    line_buffered: bool,
    mut process: F,
) -> Result<()>
where
    F: FnMut(&[u8], &mut dyn Write) -> io::Result<LineOutcome>,
{
    let mut reader = open_input(input.as_deref())?;
    let stdout = io::stdout();
    let mut writer = BufWriter::with_capacity(1 << 20, stdout.lock());

    let mut line: Vec<u8> = Vec::with_capacity(4096);
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).context("read error")?;
        if n == 0 {
            break;
        }

        let outcome = match process(&line, &mut writer) {
            Ok(o) => o,
            Err(e) if e.kind() == ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => bail!("write error: {e}"),
        };

        account(&mut progress, &outcome);

        // In line-buffered mode, push each matching line straight to stdout so
        // output (and the progress count) stays in step with what's delivered.
        if line_buffered
            && outcome.kept
            && let Err(e) = writer.flush()
        {
            if e.kind() == ErrorKind::BrokenPipe {
                return Ok(());
            }
            bail!("flush error: {e}");
        }
    }

    if let Err(e) = writer.flush()
        && e.kind() != ErrorKind::BrokenPipe
    {
        bail!("flush error: {e}");
    }

    if let Some(p) = progress.as_mut() {
        p.finish();
    }

    Ok(())
}
