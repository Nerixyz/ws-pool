//! # Generic `WebSocket` pool
//!
//! This crate provides a generic `WebSocket` pool where one [Handler] has multiple workers (connections).
//! A lot of the restart and communication is already handled.

#![warn(
    missing_docs,
    clippy::doc_markdown,
    rustdoc::broken_intra_doc_links,
    clippy::must_use_candidate
)]

mod event_loop;
mod handler;
mod worker;
mod ws;

use std::marker::PhantomData;

pub use event_loop::EventLoopAction;
pub use handler::{run_handler, spawn_handler, Handler, HandlerContext, Worker};
pub use worker::{WorkerHandler, WsContext};

/// Generic error indicating a message couldn't be send over an MPSC channel as it's already closed.
#[derive(Default, Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("channel closed")]
pub struct SendError {
    _priv: PhantomData<*const ()>,
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for SendError {
    fn from(_: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Self::default()
    }
}
