//! TS3 file transfer channel (FEATURES.md §11): the control plane runs over
//! the client connection (`ftinitupload`/`ftinitdownload` return an ftkey +
//! transfer port), the payload over a plain TCP connection keyed by that
//! ftkey.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use univox_core::error::{Error, Result};

/// One negotiated transfer, ready to move bytes.
pub struct TransferChannel {
    stream: TcpStream,
}

impl TransferChannel {
    /// Connect to the file transfer port and authenticate with the ftkey.
    /// After this, uploads write raw bytes and downloads read raw bytes.
    pub async fn connect(ip: &str, port: u16, ftkey: &str) -> Result<Self> {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(10),
            TcpStream::connect((ip, port)),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Other(format!("filetransfer connect: {e}")))?;

        // The handshake is just the ASCII ftkey. With proto=1 (set in
        // ftinitupload/download) the payload then flows directly: uploads
        // client->server, downloads server->client, no ack byte.
        stream
            .write_all(ftkey.as_bytes())
            .await
            .map_err(|e| Error::Other(format!("filetransfer handshake: {e}")))?;
        Ok(Self { stream })
    }

    /// Upload `data` and close the write side so the server finalizes.
    /// Waits for the server to close the connection (it commits the file).
    pub async fn upload(mut self, data: &[u8]) -> Result<()> {
        self.stream
            .write_all(data)
            .await
            .map_err(|e| Error::Other(format!("filetransfer upload: {e}")))?;
        self.stream
            .shutdown()
            .await
            .map_err(|e| Error::Other(format!("filetransfer close: {e}")))?;
        // Drain until the server closes; it may send a final status byte.
        let mut chunk = [0u8; 1024];
        loop {
            match tokio::time::timeout(
                Duration::from_secs(10),
                self.stream.read(&mut chunk),
            )
            .await
            {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
            }
        }
        Ok(())
    }

    /// Read the whole payload until EOF (`size` is advisory).
    pub async fn download(mut self, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size);
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            let n = self
                .stream
                .read(&mut chunk)
                .await
                .map_err(|e| Error::Other(format!("filetransfer download: {e}")))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        Ok(out)
    }
}
