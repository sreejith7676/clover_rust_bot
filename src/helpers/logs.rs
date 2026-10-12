use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use teloxide::error_handlers::ErrorHandler;
use teloxide::RequestError;
use tracing_subscriber::fmt::MakeWriter;

/// A thread-safe circular in-memory buffer for real-time log capture.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogBufferInner>>,
}

#[derive(Debug)]
struct LogBufferInner {
    lines: VecDeque<String>,
    pending: String,
    max_lines: usize,
}

impl LogBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogBufferInner {
                lines: VecDeque::with_capacity(max_lines),
                pending: String::new(),
                max_lines,
            })),
        }
    }

    /// Lock the buffer, recovering from a poisoned mutex (logging must never panic).
    fn lock(&self) -> MutexGuard<'_, LogBufferInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Retrieve the last `count` lines from the buffer.
    pub fn get_last_lines(&self, count: usize) -> Vec<String> {
        let inner = self.lock();
        inner.lines.iter().rev().take(count).rev().cloned().collect()
    }

    /// Retrieve all buffered lines joined with newlines.
    pub fn get_all_as_string(&self) -> String {
        let inner = self.lock();
        inner.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Clear all stored log lines and pending buffer.
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.lines.clear();
        inner.pending.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().lines.len()
    }
}

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.lock();

        let s = String::from_utf8_lossy(buf);
        inner.pending.push_str(&s);

        while let Some(pos) = inner.pending.find('\n') {
            let line = inner.pending[..pos].trim_end_matches('\r').to_string();
            inner.pending.drain(..=pos);
            if !line.is_empty() {
                if inner.lines.len() >= inner.max_lines {
                    inner.lines.pop_front();
                }
                inner.lines.push_back(line);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// An error handler for the Telegram update listener that:
/// 1. Backs off with an asynchronous sleep on network/polling errors, preventing
///    tight polling loops that consume 100% CPU and flood log files.
/// 2. Rate-limits error logging so disk space is not exhausted during network outages.
/// 3. Tracks consecutive failures and logs when connectivity is restored.
pub struct ListenerErrorHandler {
    last_log: tokio::sync::Mutex<Option<Instant>>,
    failure_count: AtomicU64,
}

impl ListenerErrorHandler {
    pub fn new() -> Self {
        Self {
            last_log: tokio::sync::Mutex::new(None),
            failure_count: AtomicU64::new(0),
        }
    }

    /// Notify that an update or network request succeeded, resetting failure tracking
    /// and logging recovery if there were preceding errors.
    pub fn notify_success(&self) {
        let prev = self.failure_count.swap(0, Ordering::Relaxed);
        if prev > 0 {
            log::info!(
                "Telegram update listener connection restored after {} consecutive failures.",
                prev
            );
        }
    }
}

impl Default for ListenerErrorHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorHandler<RequestError> for ListenerErrorHandler {
    fn handle_error(self: Arc<Self>, error: RequestError) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;

            let mut last_log_guard = self.last_log.lock().await;
            let should_log = match *last_log_guard {
                None => true,
                Some(instant) => instant.elapsed() >= Duration::from_secs(30),
            };

            if should_log {
                log::warn!(
                    "Telegram update listener connection error (consecutive failures: {}, retrying with 5s backoff): {}",
                    count,
                    error
                );
                *last_log_guard = Some(Instant::now());
            }

            drop(last_log_guard);

            // Throttle polling retries to avoid busy loops when offline.
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
    }
}

/// Spawns a background task that periodically verifies the log file size
/// and truncates it if it exceeds the maximum allowable threshold (15 MB),
/// guaranteeing that disk space cannot be exhausted.
pub fn spawn_log_size_guard() {
    tokio::spawn(async {
        let max_bytes: u64 = 15 * 1024 * 1024; // 15 MB
        let check_interval = Duration::from_secs(300); // 5 minutes

        loop {
            tokio::time::sleep(check_interval).await;

            let path = crate::config::LOG_FILE;
            if let Ok(metadata) = tokio::fs::metadata(path).await {
                if metadata.len() > max_bytes {
                    log::warn!(
                        "Log file '{}' size ({} bytes) exceeded safety limit of {} bytes; truncating to protect disk space.",
                        path,
                        metadata.len(),
                        max_bytes
                    );

                    match tokio::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(path)
                        .await
                    {
                        Ok(_) => log::info!("Log file '{}' safely truncated by background guard.", path),
                        Err(e) => log::error!("Background log guard failed to truncate '{}': {}", path, e),
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_buffer_operations() {
        let mut buffer = LogBuffer::new(3);

        write!(buffer, "Line 1\nLine 2\n").unwrap();
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.get_last_lines(1), vec!["Line 2"]);

        write!(buffer, "Line 3\nLine 4\n").unwrap();
        // Capped at max_lines (3)
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get_last_lines(3), vec!["Line 2", "Line 3", "Line 4"]);

        buffer.clear();
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn partial_lines_are_held_until_newline() {
        let mut buffer = LogBuffer::new(5);
        write!(buffer, "partial").unwrap();
        assert_eq!(buffer.len(), 0);
        writeln!(buffer, " line").unwrap();
        assert_eq!(buffer.get_last_lines(1), vec!["partial line"]);
    }

    #[test]
    fn test_listener_error_handler_notify_success() {
        let handler = ListenerErrorHandler::new();
        handler.failure_count.store(5, Ordering::Relaxed);
        handler.notify_success();
        assert_eq!(handler.failure_count.load(Ordering::Relaxed), 0);
    }
}
