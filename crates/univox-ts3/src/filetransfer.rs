//! TS3 file transfer channel (FEATURES.md §11): the control plane runs over
//! the client connection (`ftinitupload`/`ftinitdownload` return an ftkey +
//! transfer port), the payload over a plain TCP connection keyed by that
//! ftkey.

use std::sync::Weak;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use univox_core::error::{Error, Result};
use univox_ts3_proto::Command;

use crate::client::UdpConnection;

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
        self.write_chunk(data).await?;
        self.finish_upload().await
    }

    /// Read the whole payload until EOF (`size` is advisory).
    pub async fn download(mut self, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size);
        while let Some(chunk) = self.read_chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    // ---- streaming primitives ----

    /// Read one payload chunk (up to 16 KiB); None at EOF.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let mut chunk = vec![0u8; 16 * 1024];
        let n = self
            .stream
            .read(&mut chunk)
            .await
            .map_err(|e| Error::FileTransfer(format!("filetransfer read: {e}")))?;
        if n == 0 {
            Ok(None)
        } else {
            chunk.truncate(n);
            Ok(Some(chunk))
        }
    }

    /// Write one payload chunk.
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        self.stream
            .write_all(chunk)
            .await
            .map_err(|e| Error::FileTransfer(format!("filetransfer write: {e}")))
    }

    /// Close the write side so the server finalizes, then drain until the
    /// server closes (it may send a final status byte).
    pub async fn finish_upload(&mut self) -> Result<()> {
        self.stream
            .shutdown()
            .await
            .map_err(|e| Error::FileTransfer(format!("filetransfer close: {e}")))?;
        let mut chunk = [0u8; 1024];
        loop {
            match tokio::time::timeout(
                Duration::from_secs(10),
                self.stream.read(&mut chunk),
            )
            .await
            {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(_)) => {}
            }
        }
        Ok(())
    }
}

/// Send `ftstop` for an abandoned transfer, then drop the payload
/// channel. Ordering matters: the socket close must come AFTER `ftstop` —
/// the server treats an unannounced close as a completed transfer and
/// commits the partial file (verified on 3.13.8). Best effort: the
/// connection may already be busy or gone.
fn spawn_abort(
    conn: &Weak<UdpConnection>,
    serverftfid: u32,
    channel: Option<TransferChannel>,
    delete: u8,
) {
    let Some(conn) = conn.upgrade() else {
        drop(channel);
        return;
    };
    let cmd = Command::new("ftstop")
        .param("serverftfid", serverftfid)
        .param("delete", delete);
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => {
            rt.spawn(async move {
                // The actor serializes commands and rejects concurrent
                // ones — retry until our ftstop gets a slot (bounded).
                for _ in 0..20 {
                    match conn.exec(cmd.clone()).await {
                        Ok(_) => break,
                        Err(e) if e.to_string().contains("concurrent") => {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        Err(_) => break,
                    }
                }
                drop(channel);
            });
        }
        Err(_) => drop(channel),
    }
}

/// A streaming download in progress
/// ([`crate::ext::Ts3Ext::download_file_stream`]).
///
/// Read chunks with [`FileDownload::next_chunk`]; the transfer is finalized
/// (`ftstop`, delete=0) when the stream ends or [`FileDownload::finish`]
/// is called. Dropping the handle mid-transfer finalizes it the same way.
pub struct FileDownload {
    ft: Option<TransferChannel>,
    conn: Weak<UdpConnection>,
    serverftfid: u32,
    size: u64,
    received: u64,
    stopped: bool,
}

impl FileDownload {
    pub(crate) fn new(
        ft: TransferChannel,
        conn: Weak<UdpConnection>,
        serverftfid: u32,
        size: u64,
    ) -> Self {
        Self {
            ft: Some(ft),
            conn,
            serverftfid,
            size,
            received: 0,
            stopped: false,
        }
    }

    /// Total size in bytes, as reported by the server at negotiation.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Bytes received so far (progress).
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Next chunk (up to 16 KiB); None when the transfer is complete.
    /// Reaching the end finalizes the transfer.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(ft) = self.ft.as_mut() else {
            return Ok(None);
        };
        match ft.read_chunk().await {
            Ok(Some(chunk)) => {
                self.received += chunk.len() as u64;
                Ok(Some(chunk))
            }
            Ok(None) => {
                self.stop(0).await?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Finalize early (e.g. enough data received): `ftstop` delete=0.
    pub async fn finish(mut self) -> Result<()> {
        self.stop(0).await
    }

    async fn stop(&mut self, delete: u8) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let res = if let Some(conn) = self.conn.upgrade() {
            let cmd = Command::new("ftstop")
                .param("serverftfid", self.serverftfid)
                .param("delete", delete);
            conn.exec(cmd)
                .await
                .map(|_| ())
                .map_err(|e| Error::FileTransfer(e.to_string()))
        } else {
            Ok(())
        };
        self.ft.take(); // close the payload socket after ftstop
        res
    }
}

impl Drop for FileDownload {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        spawn_abort(&self.conn, self.serverftfid, self.ft.take(), 0);
    }
}

/// A streaming upload in progress
/// ([`crate::ext::Ts3Ext::upload_file_stream`]).
///
/// Write chunks with [`FileUpload::write_chunk`], then commit with
/// [`FileUpload::finish`]. [`FileUpload::abort`] discards the partial
/// file; dropping the handle without finishing aborts too (delete=1).
pub struct FileUpload {
    ft: Option<TransferChannel>,
    conn: Weak<UdpConnection>,
    serverftfid: u32,
    size: u64,
    written: u64,
    stopped: bool,
}

impl FileUpload {
    pub(crate) fn new(
        ft: TransferChannel,
        conn: Weak<UdpConnection>,
        serverftfid: u32,
        size: u64,
    ) -> Self {
        Self {
            ft: Some(ft),
            conn,
            serverftfid,
            size,
            written: 0,
            stopped: false,
        }
    }

    /// Total size in bytes, as announced at negotiation.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Bytes written so far (progress).
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Write one chunk; the server expects exactly `size` bytes in total.
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        let Some(ft) = self.ft.as_mut() else {
            return Err(Error::FileTransfer("upload already finished".into()));
        };
        ft.write_chunk(chunk).await?;
        self.written += chunk.len() as u64;
        Ok(())
    }

    /// Commit the upload: close the write side, wait for the server to
    /// finalize, then `ftstop` delete=0.
    ///
    /// Errors when fewer than `size` bytes were written — call
    /// [`FileUpload::abort`] instead to discard a partial upload
    /// deliberately.
    pub async fn finish(mut self) -> Result<()> {
        if self.written != self.size {
            return Err(Error::FileTransfer(format!(
                "upload incomplete: {} of {} bytes written",
                self.written, self.size
            )));
        }
        if let Some(ft) = self.ft.as_mut() {
            ft.finish_upload().await?;
        }
        self.stop(0).await
    }

    /// Discard the partial upload (`ftstop` delete=1).
    pub async fn abort(mut self) -> Result<()> {
        self.stop(1).await
    }

    async fn stop(&mut self, delete: u8) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let res = if let Some(conn) = self.conn.upgrade() {
            let cmd = Command::new("ftstop")
                .param("serverftfid", self.serverftfid)
                .param("delete", delete);
            conn.exec(cmd)
                .await
                .map(|_| ())
                .map_err(|e| Error::FileTransfer(e.to_string()))
        } else {
            Ok(())
        };
        self.ft.take(); // close the payload socket after ftstop
        res
    }
}

impl Drop for FileUpload {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Abandoned mid-upload: remove the partial file server-side.
        spawn_abort(&self.conn, self.serverftfid, self.ft.take(), 1);
    }
}
