//! Shared helpers for integration tests. `tests/common/mod.rs` (rather than
//! `tests/common.rs`) so cargo's test-target auto-discovery doesn't also
//! compile this as its own standalone test binary.

use std::sync::{Arc, Mutex};

/// Shared in-memory sink for a scoped `tracing` subscriber, so a test can
/// assert on log output without touching the process-global subscriber.
#[derive(Clone, Default)]
pub struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl LogBuf {
    pub fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
