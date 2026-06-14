#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::missing_panics_doc
)]

mod builder;
mod errors;
mod output;
mod sender;
#[cfg(test)]
mod tests;

pub use builder::Builder;
pub use errors::SendError;
use output::{Output, OutputMetrics};
pub use sender::{SourceSender, SourceSenderItem};

use std::sync::atomic::{AtomicUsize, Ordering};

/// Default number of events batched per source send and used as the base for source output buffer sizing.
pub const DEFAULT_CHUNK_SIZE: usize = 1000;

/// Default chunk size. Prefer [`DEFAULT_CHUNK_SIZE`] or [`chunk_size()`].
pub const CHUNK_SIZE: usize = DEFAULT_CHUNK_SIZE;

static CONFIGURED_CHUNK_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Errors returned by [`init_chunk_size`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitChunkSizeError {
    /// Chunk size must be greater than zero.
    Zero,
    /// Chunk size was already initialized.
    AlreadyInitialized,
}

/// Returns the configured source sender chunk size, or [`DEFAULT_CHUNK_SIZE`] if unset.
#[must_use]
pub fn chunk_size() -> usize {
    match CONFIGURED_CHUNK_SIZE.load(Ordering::Relaxed) {
        0 => DEFAULT_CHUNK_SIZE,
        size => size,
    }
}

/// Initializes the source sender chunk size. Must be called at most once before building the topology.
pub fn init_chunk_size(size: usize) -> Result<(), InitChunkSizeError> {
    if size == 0 {
        return Err(InitChunkSizeError::Zero);
    }

    CONFIGURED_CHUNK_SIZE
        .compare_exchange(0, size, Ordering::AcqRel, Ordering::Relaxed)
        .map_err(|_| InitChunkSizeError::AlreadyInitialized)?;

    Ok(())
}

#[cfg(any(test, feature = "test"))]
const TEST_BUFFER_SIZE: usize = 100;

use vector_common::internal_event::HistogramName;

const LAG_TIME_NAME: HistogramName = HistogramName::SourceLagTimeSeconds;
const SEND_LATENCY_NAME: HistogramName = HistogramName::SourceSendLatencySeconds;
const SEND_BATCH_LATENCY_NAME: HistogramName = HistogramName::SourceSendBatchLatencySeconds;
