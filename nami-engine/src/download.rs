//! Pieces shared by the streamed file downloads (the uptime check export,
//! `uptime::export`, and the log export, `logs::export`): the spreadsheet
//! formula guard for CSV cells and a response body that can end with an
//! error instead of cleanly.

use std::{
    borrow::Cow,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use axum::body::Bytes;
use tokio::sync::mpsc;

/// `cell`, with a leading `'` when a spreadsheet would read it as a formula
/// (`=`, `+`, `-`, `@`, tab or CR first).
pub fn neutralize(cell: &str) -> Cow<'_, str> {
    match cell.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => Cow::Owned(format!("'{cell}")),
        _ => Cow::Borrowed(cell),
    }
}

/// A download body fed by a producer task over a channel: the producer's
/// chunks, then a clean end, or an error when the producer set `aborted`
/// before it dropped its sender (hyper then cuts the connection instead of
/// ending the chunked body, so the client sees a failed download, never a
/// short file that looks complete).
pub struct AbortableBody {
    rx: mpsc::Receiver<Bytes>,
    aborted: Arc<AtomicBool>,
    done: bool,
}

impl AbortableBody {
    pub fn new(rx: mpsc::Receiver<Bytes>, aborted: Arc<AtomicBool>) -> Self {
        AbortableBody {
            rx,
            aborted,
            done: false,
        }
    }
}

impl tokio_stream::Stream for AbortableBody {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(None) => {
                self.done = true;
                if self.aborted.load(Ordering::Acquire) {
                    Poll::Ready(Some(Err(io::Error::other("download aborted"))))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
