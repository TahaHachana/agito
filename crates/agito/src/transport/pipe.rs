//! `--remote-debugging-pipe` connections: NUL-terminated JSON messages over two pipes.
//!
//! Used for Chromium launched by agito on Unix. Because the browser reads its commands from
//! a pipe this process holds, the browser exits when this process does, even on SIGKILL.

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use super::socket::{SocketPair, SocketReceiver, SocketSender};

const TERMINATOR: u8 = 0;

/// Wraps the pipe the browser reads commands from and the pipe it writes replies to.
pub fn pipe_pair<W, R>(to_browser: W, from_browser: R) -> SocketPair
where
    W: AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
{
    SocketPair::new(
        PipeSender {
            writer: Some(to_browser),
        },
        PipeReceiver {
            reader: BufReader::with_capacity(32 * 1024, from_browser),
        },
    )
}

struct PipeSender<W> {
    writer: Option<W>,
}

#[async_trait]
impl<W: AsyncWrite + Unpin + Send + 'static> SocketSender for PipeSender<W> {
    async fn send(&mut self, message: String) -> std::io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"))?;
        let mut bytes = message.into_bytes();
        bytes.push(TERMINATOR);
        writer.write_all(&bytes).await?;
        writer.flush().await
    }

    async fn close(&mut self) {
        // Dropping the write end is what tells the browser to go.
        if let Some(mut writer) = self.writer.take() {
            let _ = writer.shutdown().await;
        }
    }
}

struct PipeReceiver<R> {
    reader: BufReader<R>,
}

#[async_trait]
impl<R: AsyncRead + Unpin + Send + 'static> SocketReceiver for PipeReceiver<R> {
    async fn recv(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut message = Vec::new();
        let read = self.reader.read_until(TERMINATOR, &mut message).await?;
        if read == 0 {
            // The browser closed its end, which is how it says it has gone.
            return Ok(None);
        }
        if message.last() == Some(&TERMINATOR) {
            message.pop();
        } else {
            // The pipe ended part way through a message; what arrived cannot be parsed.
            return Ok(None);
        }
        Ok(Some(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, duplex};

    #[tokio::test]
    async fn sends_nul_terminated_messages() {
        let (ours, mut theirs) = duplex(1024);
        let (_unused, read_half) = duplex(16);
        let mut pair = pipe_pair(ours, read_half);
        pair.sender.send("{\"id\":1}".into()).await.unwrap();
        let mut buf = [0u8; 9];
        theirs.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"{\"id\":1}\0");
    }

    #[tokio::test]
    async fn splits_messages_across_and_within_reads() {
        let (mut browser, ours) = duplex(1024);
        let (write_half, _unused) = duplex(16);
        let mut pair = pipe_pair(write_half, ours);
        browser.write_all(b"{\"a\":1}\0{\"b\"").await.unwrap();
        browser.write_all(b":2}\0").await.unwrap();
        assert_eq!(pair.receiver.recv().await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(pair.receiver.recv().await.unwrap().unwrap(), b"{\"b\":2}");
        drop(browser);
        assert!(pair.receiver.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn large_message_is_reassembled() {
        let (mut browser, ours) = duplex(64 * 1024);
        let (write_half, _unused) = duplex(16);
        let mut pair = pipe_pair(write_half, ours);
        let big = format!("{{\"data\":\"{}\"}}", "x".repeat(200_000));
        let expected = big.clone();
        tokio::spawn(async move {
            browser.write_all(big.as_bytes()).await.unwrap();
            browser.write_all(&[0]).await.unwrap();
        });
        assert_eq!(
            pair.receiver.recv().await.unwrap().unwrap(),
            expected.as_bytes()
        );
    }
}
