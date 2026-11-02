//! Chunked playout orchestration: synthesize ahead while the device drains.
//!
//! One worker thread synthesizes chunks in order; the caller plays each
//! chunk as it arrives. Playback of chunk N overlaps synthesis of chunk
//! N+1, so first audio lands after the first chunk instead of the whole
//! turn. Order is structural (channel order), never timing.

/// Play `chunks` through `play` as `synth` produces them, in order.
///
/// The worker synthesizes ahead through a bounded channel (at most two
/// chunks in flight); the caller blocks on the next chunk and plays it
/// immediately. A synth error stops the stream with that error; a play
/// error cancels synthesis promptly (the worker observes the dropped
/// receiver and stops). A dead worker re-raises its panic — speech
/// must never truncate silently.
pub fn stream_chunks<S, P, E>(chunks: Vec<String>, synth: S, mut play: P) -> Result<(), E>
where
    S: Fn(&str) -> Result<Vec<f32>, E> + Send + 'static,
    P: FnMut(&[f32]) -> Result<(), E>,
    E: Send + 'static,
{
    if chunks.is_empty() {
        return Ok(());
    }
    /// In-flight chunk cap: one playing plus one synthesizing.
    const IN_FLIGHT: usize = 2;
    let count = chunks.len();
    let (tx, rx) = std::sync::mpsc::sync_channel(IN_FLIGHT);
    let worker = std::thread::spawn(move || {
        for chunk in &chunks {
            if tx.send(synth(chunk)).is_err() {
                break;
            }
        }
    });
    let mut outcome = Ok(());
    for _ in 0..count {
        match rx.recv() {
            Ok(Ok(samples)) => {
                if let Err(error) = play(&samples) {
                    outcome = Err(error);
                    break;
                }
            }
            Ok(Err(error)) => {
                outcome = Err(error);
                break;
            }
            Err(_) => break,
        }
    }
    // Early break leaves the worker blocked on a full channel; dropping
    // the receiver unblocks its send so `join` cannot hang.
    drop(rx);
    match worker.join() {
        Ok(()) => outcome,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(test)]
#[path = "../tests/stream.rs"]
mod tests;
