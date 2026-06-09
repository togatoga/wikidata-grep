//! Parallel processing pipeline with order-preserving output.
//!
//! A reader thread slices stdin into blocks of whole lines, worker threads
//! process each block independently (via a caller-supplied per-line closure),
//! and the main thread reorders the finished blocks by sequence number before
//! writing them — so the output is byte-for-byte identical to the sequential
//! path regardless of thread count.
//!
//! The engine is generic over the per-line work: callers pass a `process`
//! closure `(line, &mut out) -> LineOutcome`. Both the filter path and the
//! `build-graph` subcommand reuse it.

use std::collections::HashMap;
use std::io::{self, BufWriter, ErrorKind, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel, sync_channel};
use std::sync::{Mutex, mpsc};
use std::thread;

use anyhow::{Result, anyhow};

use crate::process::LineOutcome;
use crate::progress::ProgressBar;

/// Target block size handed to each worker (whole lines only).
const BLOCK_TARGET: usize = 4 * 1024 * 1024;

struct Block {
    seq: usize,
    data: Vec<u8>,
}

struct Done {
    seq: usize,
    out: Vec<u8>,
    total: u64,
    kept: u64,
    last_id: Option<String>,
}

/// Run the order-preserving parallel pipeline, applying `process` to every
/// non-empty line. `process` writes any output for the line into the supplied
/// buffer and returns a [`LineOutcome`] describing what happened (used for
/// progress accounting). It must be cheap to share across threads.
pub fn run<P>(
    workers: usize,
    mut progress: Option<ProgressBar>,
    input: Option<String>,
    process: P,
) -> Result<()>
where
    P: Fn(&[u8], &mut Vec<u8>) -> LineOutcome + Send + Sync + 'static,
{
    let process = Arc::new(process);

    // Bounded work queue gives back-pressure so memory stays bounded.
    let (work_tx, work_rx) = sync_channel::<Block>(workers * 2);
    let work_rx = Arc::new(Mutex::new(work_rx));
    let (done_tx, done_rx) = channel::<Done>();

    // Set when the writer stops early (downstream closed, e.g. `| head`); the
    // reader checks it so it stops slurping input instead of draining the whole
    // file after nobody is reading our output anymore.
    let stop = Arc::new(AtomicBool::new(false));

    let worker_handles = spawn_workers(workers, &work_rx, &done_tx, &process);
    drop(done_tx); // done_rx ends once every worker has dropped its sender

    let reader_stop = Arc::clone(&stop);
    let reader_handle = thread::spawn(move || reader_loop(work_tx, input, &reader_stop));

    // Writer: reorder finished blocks by sequence and emit them in order.
    let stdout = io::stdout();
    let mut writer = BufWriter::with_capacity(1 << 20, stdout.lock());
    let mut next: usize = 0;
    let mut pending: HashMap<usize, Done> = HashMap::new();
    let mut result: Result<()> = Ok(());

    'outer: for done in done_rx {
        pending.insert(done.seq, done);
        while let Some(d) = pending.remove(&next) {
            if let Err(e) = writer.write_all(&d.out) {
                // Tell the reader to stop slurping input: without this it would
                // read the rest of the file even though nobody is consuming our
                // output, so `| head` on a huge dump appears to hang.
                stop.store(true, Ordering::Relaxed);
                if e.kind() == ErrorKind::BrokenPipe {
                    // Downstream closed (e.g. `| head`). Stop; dropping done_rx
                    // cascades shutdown to workers and the reader.
                    break 'outer;
                }
                result = Err(anyhow!("write error: {e}"));
                break 'outer;
            }
            if let Some(p) = progress.as_mut() {
                p.add_block(d.total, d.kept, d.last_id.as_deref());
            }
            next += 1;
        }
    }

    if result.is_ok()
        && let Err(e) = writer.flush()
        && e.kind() != ErrorKind::BrokenPipe
    {
        result = Err(anyhow!("flush error: {e}"));
    }

    if let Some(p) = progress.as_mut() {
        p.finish();
    }

    // Drain the queue so the reader can unblock, then join everything.
    drop(pending);
    let _ = reader_handle.join();
    for h in worker_handles {
        let _ = h.join();
    }

    result
}

fn spawn_workers<P>(
    workers: usize,
    work_rx: &Arc<Mutex<Receiver<Block>>>,
    done_tx: &mpsc::Sender<Done>,
    process: &Arc<P>,
) -> Vec<thread::JoinHandle<()>>
where
    P: Fn(&[u8], &mut Vec<u8>) -> LineOutcome + Send + Sync + 'static,
{
    (0..workers)
        .map(|_| {
            let work_rx = Arc::clone(work_rx);
            let done_tx = done_tx.clone();
            let process = Arc::clone(process);
            thread::spawn(move || {
                loop {
                    // Hold the lock only for the recv, not while processing.
                    let block = {
                        let rx = work_rx.lock().unwrap();
                        rx.recv()
                    };
                    let Ok(block) = block else { break };

                    let mut out = Vec::with_capacity(block.data.len() / 4 + 64);
                    let mut total = 0u64;
                    let mut kept = 0u64;
                    let mut last_id = None;
                    for line in block.data.split(|&b| b == b'\n') {
                        if line.is_empty() {
                            continue;
                        }
                        let o = process(line, &mut out);
                        if o.is_entity {
                            total += 1;
                        }
                        if o.kept {
                            kept += 1;
                            if o.last_id.is_some() {
                                last_id = o.last_id;
                            }
                        }
                    }

                    let done = Done {
                        seq: block.seq,
                        out,
                        total,
                        kept,
                        last_id,
                    };
                    if done_tx.send(done).is_err() {
                        break;
                    }
                }
            })
        })
        .collect()
}

fn reader_loop(
    work_tx: mpsc::SyncSender<Block>,
    input: Option<String>,
    stop: &AtomicBool,
) -> io::Result<()> {
    use std::io::BufRead;
    let mut reader = crate::runner::open_input(input.as_deref()).map_err(io::Error::other)?;
    let mut seq = 0usize;
    let mut block = Vec::with_capacity(BLOCK_TARGET + 65536);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let n = reader.read_until(b'\n', &mut block)?;
        if n == 0 {
            if !block.is_empty() {
                let _ = work_tx.send(Block {
                    seq,
                    data: std::mem::take(&mut block),
                });
            }
            break;
        }
        if block.len() >= BLOCK_TARGET {
            if work_tx
                .send(Block {
                    seq,
                    data: std::mem::take(&mut block),
                })
                .is_err()
            {
                break;
            }
            seq += 1;
            block = Vec::with_capacity(BLOCK_TARGET + 65536);
        }
    }
    Ok(())
}
