//! Async tokio bridge over [`graticule::InputReader`].

use std::io;

use graticule::{Event, InputReader, KeyEventKind, ProbeHandle};
use tokio::sync::mpsc;

/// A caller-owned terminal input stream.
///
/// The attach loop (`runner::run_attached`) keeps one of these alive across
/// re-attaches so events that arrive while the next session handshakes are
/// delivered to the next loop instead of being dropped with the previous one.
/// The reader thread is owned here: dropping the source stops it.
pub(crate) struct InputSource {
    rx: mpsc::UnboundedReceiver<Event>,
    reader: InputReader,
}

impl InputSource {
    /// Start the reader thread that feeds this source.
    pub(crate) fn spawn() -> io::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = InputReader::spawn_with(move |ev| {
            if let Event::Key(k) = &ev
                && k.kind == KeyEventKind::Release
            {
                return true;
            }
            tx.send(ev).is_ok()
        })?;
        Ok(Self { rx, reader })
    }

    /// Return a cloneable handle for off-frame capability probes.
    pub(crate) fn probe_handle(&self) -> ProbeHandle {
        self.reader.probe_handle()
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
