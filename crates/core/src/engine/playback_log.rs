//! 재생 완료 로그 배치 전송.

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::json;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use crate::cms::{CmsApiClient, PlaybackLogItem};
use crate::timeline::PlaybackScene;

const BATCH_SIZE: usize = 10;
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_QUEUE_ITEMS: usize = 1_000;

/// 전송 큐에 넣기 전 scene에서 뽑아 둔 로그용 메타데이터.
#[derive(Debug, Clone)]
pub(crate) struct PlaybackLogScene {
    pub scene_id: String,
    pub content_id: i64,
    pub schedule_id: i64,
    pub playlist_id: i64,
    pub item_id: i64,
    pub start_time_millis: i64,
    pub end_time_millis: i64,
}

impl PlaybackLogScene {
    pub fn from_scene(scene: &PlaybackScene) -> Option<Self> {
        let content_id = scene.layout.as_ref().map(|layout| layout.id)?;
        Some(Self {
            scene_id: scene.scene_id.clone(),
            content_id,
            schedule_id: scene.schedule_id,
            playlist_id: scene.playlist_id,
            item_id: scene.item_id,
            start_time_millis: scene.start_time_millis,
            end_time_millis: scene.end_time_millis,
        })
    }
}

/// 현재 화면에 표출 중인 scene의 재생 로그 상태.
#[derive(Debug, Clone)]
pub(crate) struct ActivePlaybackLog {
    scene: PlaybackLogScene,
    started_at_millis: i64,
}

impl ActivePlaybackLog {
    pub fn new(scene: PlaybackLogScene, started_at_millis: i64) -> Self {
        Self {
            scene,
            started_at_millis,
        }
    }

    pub fn scene_id(&self) -> &str {
        &self.scene.scene_id
    }

    pub fn end_time_millis(&self) -> i64 {
        self.scene.end_time_millis
    }

    pub fn into_item(
        self,
        device_id: &str,
        ended_at_millis: i64,
        completed: bool,
    ) -> Option<PlaybackLogItem> {
        let ended_at_millis = ended_at_millis.max(self.started_at_millis);
        Some(PlaybackLogItem {
            device_id: device_id.to_string(),
            content_type: "layout".to_string(),
            content_id: self.scene.content_id,
            started_at: iso8601_millis(self.started_at_millis)?,
            ended_at: iso8601_millis(ended_at_millis)?,
            completed,
            extra: json!({
                "scene_id": self.scene.scene_id,
                "schedule_id": self.scene.schedule_id,
                "playlist_id": self.scene.playlist_id,
                "item_id": self.scene.item_id,
                "scheduled_start_time_millis": self.scene.start_time_millis,
                "scheduled_end_time_millis": self.scene.end_time_millis,
            }),
        })
    }
}

/// 재생 로그를 비동기 배치로 전송하는 큐.
pub(crate) struct PlaybackLogReporter {
    tx: UnboundedSender<PlaybackLogItem>,
    rx: StdMutex<Option<UnboundedReceiver<PlaybackLogItem>>>,
}

impl PlaybackLogReporter {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: StdMutex::new(Some(rx)),
        }
    }

    /// 백그라운드 flush 태스크를 시작한다. 중복 호출은 무시한다.
    pub fn start(&self, cms: CmsApiClient) {
        let Ok(mut rx) = self.rx.lock() else {
            warn!("playback log reporter receiver lock poisoned");
            return;
        };
        let Some(rx) = rx.take() else {
            return;
        };
        tokio::spawn(run_reporter(cms, rx));
    }

    /// 로그 한 건을 큐에 넣는다. 큐 전송 실패는 앱 종료 중인 상황으로 보고 무시한다.
    pub fn record(&self, item: PlaybackLogItem) {
        let _ = self.tx.send(item);
    }
}

async fn run_reporter(cms: CmsApiClient, mut rx: UnboundedReceiver<PlaybackLogItem>) {
    let mut pending = VecDeque::new();
    let mut interval = tokio::time::interval(FLUSH_INTERVAL);

    loop {
        tokio::select! {
            Some(item) = rx.recv() => {
                pending.push_back(item);
                trim_queue(&mut pending);
                if pending.len() >= BATCH_SIZE {
                    flush_pending(&cms, &mut pending).await;
                }
            }
            _ = interval.tick() => {
                flush_pending(&cms, &mut pending).await;
            }
            else => {
                flush_pending(&cms, &mut pending).await;
                break;
            }
        }
    }
}

async fn flush_pending(cms: &CmsApiClient, pending: &mut VecDeque<PlaybackLogItem>) {
    if pending.is_empty() {
        return;
    }
    let items: Vec<_> = pending.iter().take(BATCH_SIZE).cloned().collect();
    match cms.post_playback_logs_batch(&items).await {
        Ok(()) => {
            debug!(count = items.len(), "playback logs batch sent");
            for _ in 0..items.len() {
                pending.pop_front();
            }
        }
        Err(err) => {
            warn!(count = items.len(), error = %err, "playback logs batch failed");
        }
    }
}

fn trim_queue(pending: &mut VecDeque<PlaybackLogItem>) {
    while pending.len() > MAX_QUEUE_ITEMS {
        pending.pop_front();
        warn!("dropping oldest playback log because queue is full");
    }
}

fn iso8601_millis(millis: i64) -> Option<String> {
    Utc.timestamp_millis_opt(millis)
        .single()
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}
