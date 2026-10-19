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

use axum::{
    Json,
    body::Bytes,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::sync::mpsc;

/// `Retry-After` of a refused download.
pub const BUSY_RETRY_AFTER_SECS: &str = "5";

/// A download refused for want of a slot. One convention for every export:
/// the user's own cap is a 429 ([`user_busy`]), the instance being full a
/// 503 ([`instance_busy`]); both say when to retry.
fn busy(status: StatusCode, message: &str) -> Response {
    (
        status,
        [
            (header::RETRY_AFTER, BUSY_RETRY_AFTER_SECS),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// 429: the user already runs `max` downloads of this kind.
pub fn user_busy(max: usize) -> Response {
    busy(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("you already have {max} exports running; retry when one finishes"),
    )
}

/// 503: every download slot of the instance is taken.
pub fn instance_busy() -> Response {
    busy(StatusCode::SERVICE_UNAVAILABLE, "busy, retry shortly")
}

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
