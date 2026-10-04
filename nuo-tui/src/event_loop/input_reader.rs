//! Dedicated terminal-input reader thread and SGR mouse fragment reassembly.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{self, Event};
use tokio::sync::mpsc;

use crate::input::{self, Feed};

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
            .name("mutx-engine-input".into())
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
