//! Batch mode options for recording and replaying transfers.

use std::sync::{Arc, Mutex};

use super::types::LocalCopyOptions;
use crate::batch::BatchWriter;

impl LocalCopyOptions {
    /// Sets the batch writer for recording transfer operations.
    ///
    /// When a batch writer is provided, the execution layer will record
    /// file list and delta operations to the batch file for later replay.
    ///
    /// # Example
    /// ```ignore
    /// use engine::batch::{BatchConfig, BatchMode, BatchWriter};
    /// use engine::local_copy::LocalCopyOptions;
    /// use std::sync::{Arc, Mutex};
    ///
    /// let config = BatchConfig::new(
    ///     BatchMode::Write,
    ///     "mybatch".to_string(),
    ///     32,
    /// );
    /// let writer = BatchWriter::new(config).expect("create writer");
    /// let options = LocalCopyOptions::new()
    ///     .batch_writer(Some(Arc::new(Mutex::new(writer))));
    /// ```
    #[must_use]
    pub fn batch_writer(mut self, writer: Option<Arc<Mutex<BatchWriter>>>) -> Self {
        self.batch_writer = writer;
        self
    }
}
