use std::collections::hash_map::{Entry, HashMap};
use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

use serde_json::Value;

use bitcoin::Txid;

#[macro_use]
mod macros;

pub mod banner;
pub mod bitcoincore_ext;
pub mod descriptor;
pub mod xpub;

pub use bitcoincore_ext::RpcApiExt;

const VSIZE_BIN_WIDTH: u32 = 50_000; // vbytes

// Make the fee histogram out of a list of `getrawmempool true` entries
pub fn make_fee_histogram(mempool_entries: HashMap<Txid, Value>) -> Vec<(f32, u32)> {
    let mut entries: Vec<_> = mempool_entries
        .into_iter()
        .map(|(_, entry)| {
            let vsize = entry["vsize"]
                .as_u64()
                .or_else(|| entry["size"].as_u64())
                .unwrap(); // bitcoind is borked if this fails
            let fee = entry["fee"].as_f64().unwrap();
            let feerate = fee as f32 / vsize as f32 * 100_000_000f32;
            (vsize as u32, feerate)
        })
        .collect();

    // XXX should take unconfirmed parents feerates into account

    entries.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let mut histogram = vec![];
    let mut bin_size = 0;
    let mut last_feerate = 0.0;

    for (vsize, feerate) in entries.into_iter().rev() {
        if bin_size > VSIZE_BIN_WIDTH && (last_feerate - feerate).abs() > f32::EPSILON {
            // vsize of transactions paying >= last_feerate
            histogram.push((last_feerate, bin_size));
            bin_size = 0;
        }
        bin_size += vsize;
        last_feerate = feerate;
    }

    if bin_size > 0 {
        histogram.push((last_feerate, bin_size));
    }

    histogram
}

pub fn remove_if<K, V>(hm: &mut HashMap<K, V>, key: K, predicate: impl Fn(&mut V) -> bool) -> bool
where
    K: Eq + std::hash::Hash,
{
    if let Entry::Occupied(mut entry) = hm.entry(key) {
        if predicate(entry.get_mut()) {
            entry.remove_entry();
        }
        true
    } else {
        false
    }
}

// debounce a Sender to only emit events sent when `duration` seconds has passed since
// the previous event, or after `duration` seconds elapses without new events coming in.
pub fn debounce_sender(forward_tx: mpsc::Sender<()>, duration: u64) -> mpsc::Sender<()> {
    let duration = Duration::from_secs(duration);
    let (debounce_tx, debounce_rx) = mpsc::channel();

    thread::spawn(move || {
        'outer: loop {
            let tick_start = Instant::now();
            // always wait for the first sync message to arrive first
            if debounce_rx.recv().is_err() {
                break 'outer;
            }
            if tick_start.elapsed() < duration {
                // if duration hasn't passed, debounce for another `duration` seconds
                loop {
                    trace!(target: "bwt::real-time", "debouncing sync for {:?}", duration);
                    match debounce_rx.recv_timeout(duration) {
                        // if we receive another message within the `duration`, debounce and start over again
                        Ok(()) => continue,
                        // if we timed-out, we're good!
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break 'outer,
                    }
                }
            }
            debug!(target: "bwt::real-time", "triggering real-time index sync");
            if forward_tx.send(()).is_err() {
                break 'outer;
            }
        }
        trace!(target: "bwt::real-time", "debounce sync thread shutting down");
    });

    debounce_tx
}

/// Wait for the future to resolve, blocking the current thread until it does
#[cfg(feature = "tokio")]
pub fn block_on_future<F: std::future::Future>(future: F) -> F::Output {
    let mut rt = tokio::runtime::Builder::new()
        .basic_scheduler()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(future)
}

pub trait BoolThen {
    // Similar to https://doc.rust-lang.org/std/primitive.bool.html#method.then (nightly only)
    fn do_then<T>(self, f: impl FnOnce() -> T) -> Option<T>;

    // Alternative version where the closure returns an Option<T>
    fn and_then<T>(self, f: impl FnOnce() -> Option<T>) -> Option<T>;
}

impl BoolThen for bool {
    fn do_then<T>(self, f: impl FnOnce() -> T) -> Option<T> {
        if self {
            Some(f())
        } else {
            None
        }
    }
    fn and_then<T>(self, f: impl FnOnce() -> Option<T>) -> Option<T> {
        if self {
            f()
        } else {
            None
        }
    }
}
