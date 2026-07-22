//! RTB tracking beacon 비동기 전송 큐.

use std::collections::HashSet;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::timeline::{RtbSceneMetadata, TrackingEvent};

const QUEUE_CAPACITY: usize = 1_024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug)]
struct TrackingRequest {
    key: String,
    event: TrackingEvent,
    url: String,
}

enum TrackingCommand {
    Send(TrackingRequest),
    Shutdown(oneshot::Sender<()>),
}

/// 렌더/재생 경로에서는 `try_send`만 수행하는 tracking reporter.
pub(crate) struct TrackingReporter {
    tx: mpsc::Sender<TrackingCommand>,
    rx: StdMutex<Option<mpsc::Receiver<TrackingCommand>>>,
    client: Client,
    dedup: StdMutex<HashSet<String>>,
    worker: StdMutex<Option<JoinHandle<()>>>,
}

impl TrackingReporter {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to create tracking HTTP client")?;
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        Ok(Self {
            tx,
            rx: StdMutex::new(Some(rx)),
            client,
            dedup: StdMutex::new(HashSet::new()),
            worker: StdMutex::new(None),
        })
    }

    /// 백그라운드 워커를 한 번만 시작한다.
    pub fn start(&self) {
        let Some(rx) = self.rx.lock().ok().and_then(|mut rx| rx.take()) else {
            return;
        };
        let client = self.client.clone();
        let handle = tokio::spawn(run_worker(client, rx));
        if let Ok(mut worker) = self.worker.lock() {
            *worker = Some(handle);
        }
    }

    /// 한 이벤트에 등록된 URL들을 non-blocking으로 큐에 넣는다.
    pub fn record(&self, metadata: &RtbSceneMetadata, scene_id: &str, event: TrackingEvent) {
        for tracking in metadata
            .tracking
            .iter()
            .filter(|tracking| tracking.event == event)
        {
            let key = format!(
                "{}|{}|{}|{}",
                metadata.slot_id,
                scene_id,
                event.as_str(),
                tracking.url
            );
            let inserted = self
                .dedup
                .lock()
                .map(|mut dedup| dedup.insert(key.clone()))
                .unwrap_or(false);
            if !inserted {
                continue;
            }
            let request = TrackingRequest {
                key: key.clone(),
                event,
                url: tracking.url.clone(),
            };
            if let Err(err) = self.tx.try_send(TrackingCommand::Send(request)) {
                if let Ok(mut dedup) = self.dedup.lock() {
                    dedup.remove(&key);
                }
                warn!(
                    slot_id = %metadata.slot_id,
                    scene_id,
                    event = event.as_str(),
                    error = %err,
                    "tracking queue full or closed"
                );
            }
        }
    }

    /// 앞서 enqueue된 요청을 drain한 뒤 워커를 종료한다.
    pub async fn shutdown(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self
            .tx
            .send(TrackingCommand::Shutdown(done_tx))
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, done_rx).await;
        }
        let handle = self.worker.lock().ok().and_then(|mut worker| worker.take());
        if let Some(mut handle) = handle {
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut handle)
                .await
                .is_err()
            {
                handle.abort();
                warn!("tracking worker shutdown timed out");
            }
        }
    }
}

async fn run_worker(client: Client, mut rx: mpsc::Receiver<TrackingCommand>) {
    while let Some(command) = rx.recv().await {
        match command {
            TrackingCommand::Send(request) => send_with_retry(&client, request).await,
            TrackingCommand::Shutdown(done) => {
                rx.close();
                while let Some(TrackingCommand::Send(request)) = rx.recv().await {
                    send_with_retry(&client, request).await;
                }
                let _ = done.send(());
                break;
            }
        }
    }
}

async fn send_with_retry(client: &Client, request: TrackingRequest) {
    for attempt in 1..=MAX_ATTEMPTS {
        let result = client
            .get(&request.url)
            .send()
            .await
            .and_then(|response| response.error_for_status().map(|_| ()));
        match result {
            Ok(()) => {
                debug!(
                    key = %request.key,
                    event = request.event.as_str(),
                    attempt,
                    "tracking beacon sent"
                );
                return;
            }
            Err(err) if attempt < MAX_ATTEMPTS => {
                warn!(
                    key = %request.key,
                    event = request.event.as_str(),
                    attempt,
                    error = %err,
                    "tracking beacon failed, retrying"
                );
                let factor = 1u32 << (attempt - 1);
                tokio::time::sleep(INITIAL_BACKOFF * factor).await;
            }
            Err(err) => {
                warn!(
                    key = %request.key,
                    event = request.event.as_str(),
                    attempt,
                    error = %err,
                    "tracking beacon permanently failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::timeline::TrackingUrl;

    #[tokio::test]
    async fn retries_deduplicates_and_drains_on_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let server = tokio::spawn(async move {
            for attempt in 1..=3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await.unwrap();
                server_requests.fetch_add(1, Ordering::SeqCst);
                let status = if attempt < 3 {
                    "500 Internal Server Error"
                } else {
                    "204 No Content"
                };
                let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let reporter = TrackingReporter::new().unwrap();
        reporter.start();
        let metadata = RtbSceneMetadata {
            slot_id: "slot-1".into(),
            request_id: None,
            bid_id: "bid-1".into(),
            imp_id: "1".into(),
            ad_id: "ad-1".into(),
            creative_id: "creative-1".into(),
            price: None,
            currency: "KRW".into(),
            tracking: vec![TrackingUrl {
                event: TrackingEvent::Start,
                url: format!("http://{address}/start"),
            }],
        };
        reporter.record(&metadata, "scene-1", TrackingEvent::Start);
        reporter.record(&metadata, "scene-1", TrackingEvent::Start);
        reporter.shutdown().await;
        server.await.unwrap();

        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }
}
