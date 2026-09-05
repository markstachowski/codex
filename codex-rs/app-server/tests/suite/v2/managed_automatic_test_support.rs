use anyhow::Result;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const PROXY_UNAVAILABLE: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// A task-local HTTPS proxy that rejects every connection without forwarding it.
pub(super) struct RejectingHttpsProxy {
    uri: String,
    accept_task: JoinHandle<()>,
}

impl RejectingHttpsProxy {
    pub(super) async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let uri = format!("http://{}", listener.local_addr()?);
        let accept_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                if stream.read_line(&mut request).await.is_ok() {
                    let mut stream = stream.into_inner();
                    let _ = stream.write_all(PROXY_UNAVAILABLE).await;
                    let _ = stream.shutdown().await;
                }
            }
        });
        Ok(Self { uri, accept_task })
    }

    pub(super) fn uri(&self) -> &str {
        &self.uri
    }
}

impl Drop for RejectingHttpsProxy {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}
