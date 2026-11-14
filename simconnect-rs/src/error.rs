//! Typed errors. This is a library crate, so callers get an enum to match on
//! rather than an opaque `anyhow::Error`.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not reach the SimConnect server at {addr}: {source}")]
    Connect {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("connection lost: {0}")]
    Io(#[from] std::io::Error),

    #[error("the sim did not answer the handshake within {0:?}")]
    HandshakeTimeout(Duration),

    #[error("the sim sent a malformed RECV_OPEN payload")]
    MalformedOpen,

    #[error(
        "sim version {major}.{minor}.{build} is older than the {min_major}.{min_minor}.{min_build} \
         required for a reliable SimVar API"
    )]
    SimTooOld {
        major: u32,
        minor: u32,
        build: u32,
        min_major: u32,
        min_minor: u32,
        min_build: u32,
    },

    /// A framed packet declared a length outside the accepted range.
    ///
    /// Almost always means the stream has desynchronised — worth surfacing
    /// rather than trying to resynchronise, because the reconnect path recovers
    /// cleanly and a resync heuristic can silently deliver garbage.
    #[error("packet length {len} is out of range ({min}..={max}) — stream desynchronised")]
    BadPacketLength { len: usize, min: usize, max: usize },
}

pub type Result<T> = std::result::Result<T, Error>;
