//! Dedicated terminal-input reader thread and SGR mouse fragment reassembly.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{self, Event};
use tokio::sync::mpsc;

use crate::shell::input::{self, Feed};

/// A caller-owned terminal input stream.
///
/// The attach loop (`runner::run_attached`) keeps one of these alive across
/// re-attaches so events that arrive while the next session handshakes are
/// delivered to the next loop instead of being dropped with the previous one.
/// The reader thread is owned here: dropping the source stops it.
pub(crate) struct InputSource {
    rx: mpsc::UnboundedReceiver<Event>,
    /// Kept only for its `Drop` (which stops the reader thread).
    _reader: InputReader,
}

impl InputSource {
    /// Start the reader thread that feeds this source.
    pub(crate) fn spawn() -> io::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = InputReader::spawn(tx)?;
        Ok(Self {
            rx,
            _reader: reader,
        })
    }

    /// The next terminal event, or `None` once the reader thread is gone.
    pub(crate) async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Drain whatever is already buffered without waiting.
    pub(crate) fn try_recv(&mut self) -> Result<Event, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}

pub(crate) struct InputReader {
    shutdown: Arc<AtomicBool>,
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

impl InputReader {
    pub(crate) fn spawn(tx: mpsc::UnboundedSender<Event>) -> io::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("nuotc-input".into())
            .spawn(move || {
                let mut sink = SgrReassemblySink::new(tx);
                while !thread_shutdown.load(Ordering::Acquire) {
                    match event::poll(std::time::Duration::from_millis(200)) {
                        Ok(true) => match event::read() {
                            Ok(ev) => {
                                if let Event::Key(k) = &ev
                                    && k.kind == crossterm::event::KeyEventKind::Release
                                {
                                    continue;
                                }
                                if !sink.handle(ev) {
                                    break;
                                }
                            }
                            Err(_) => break,
                        },
                        Ok(false) => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self { shutdown })
    }
}

struct SgrReassemblySink {
    tx: mpsc::UnboundedSender<Event>,
    guard: input::SgrLeakGuard,
}

impl SgrReassemblySink {
    fn new(tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            tx,
            guard: input::SgrLeakGuard::default(),
        }
    }

    fn handle(&mut self, ev: Event) -> bool {
        match self.guard.feed(&ev) {
            Feed::Accept => self.tx.send(ev).is_ok(),
            Feed::Drop => true,
        }
    }
}
