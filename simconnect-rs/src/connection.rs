//! TCP framing: length-prefixed SimConnect packet send and receive.

use crate::error::{Error, Result};
use crate::protocol::{self, RECV_HEADER_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest packet accepted off the wire.
///
/// A `STRING256` datum is ~300 bytes, so anything approaching this is a
/// desynchronised stream rather than a large message.
const MAX_PACKET: usize = 65_536;

pub struct SimConnectTcp {
    stream: TcpStream,
}

impl SimConnectTcp {
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|source| Error::Connect {
                addr: addr.to_string(),
                source,
            })?;
        // Telemetry is small and latency-sensitive; Nagle would coalesce
        // per-frame packets into bursts.
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }

    pub async fn send_packet(&mut self, pkt: &[u8]) -> Result<()> {
        self.stream.write_all(pkt).await?;
        Ok(())
    }

    /// Read one packet, returning `(msg_id, payload)` with the header stripped.
    ///
    /// Note the asymmetry with [`protocol::build_packet`]: outgoing packets
    /// carry a 16-byte header, but only 12 bytes are stripped here. The fourth
    /// header word is left on the front of the payload, and that is precisely
    /// why the first datum lands at [`protocol::DATUM_OFFSET`] rather than 24.
    /// Do not "fix" this without moving that offset too.
    pub async fn recv_packet(&mut self) -> Result<(u32, Vec<u8>)> {
        let mut header = [0u8; RECV_HEADER_LEN];
        self.stream.read_exact(&mut header).await?;

        let size = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let msg_id = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if !(RECV_HEADER_LEN..=MAX_PACKET).contains(&size) {
            return Err(Error::BadPacketLength {
                len: size,
                min: RECV_HEADER_LEN,
                max: MAX_PACKET,
            });
        }

        let mut payload = vec![0u8; size - RECV_HEADER_LEN];
        if !payload.is_empty() {
            self.stream.read_exact(&mut payload).await?;
        }
        Ok((msg_id, payload))
    }

    /// Send a framed message in one call.
    pub async fn send(&mut self, msg_id: u32, payload: &[u8]) -> Result<()> {
        self.send_packet(&protocol::build_packet(msg_id, payload))
            .await
    }
}
