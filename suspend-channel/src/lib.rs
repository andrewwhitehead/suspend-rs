//! Channel and stream primitives.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

pub use self::{
    async_stream::{
        make_stream, AsyncStream, AsyncStreamScope, AsyncStreamSend, TryAsyncStreamSend,
    },
    channel::{channel, send_once, Flush, ReceiveOnce, Receiver, SendOnce, Sender},
    error::RecvError,
    util::{NextFuture, StreamNext},
};

#[macro_use]
mod async_stream;

mod channel;
mod error;

mod util;
