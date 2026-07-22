//! 재생 엔진: 동기화 → 다운로드 → 준비 → 전환 오케스트레이션.
//!
//! 파일 구성:
//! - [`state`]: 엔진 상태머신([`EngineState`])과 이벤트([`EngineEvent`]) 정의
//! - [`sync`]: NTP/CMS 동기화 루프와 에셋 선다운로드 (`impl PlaybackEngine`)
//! - [`playback`]: 장면 재생 루프와 switch 결과 콜백 (`impl PlaybackEngine`)
//!
//! 엔진은 렌더러를 직접 알지 못한다. 표출 시점이 다가오면
//! [`SwitchCommand`]를 채널로 보내고, 렌더 스레드(winit 루프)가 이를 받아
//! scene prepare + 레이어 전환을 수행한다. 결과는
//! [`PlaybackEngine::on_scene_switched`] / [`PlaybackEngine::on_switch_failed`]로 회신된다.

mod playback;
mod playback_log;
mod state;
mod sync;

pub use state::{EngineEvent, EngineState, SwitchCommand};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use anyhow::Result;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use crate::cache::AssetStore;
use crate::clock::SignageClock;
use crate::cms::CmsApiClient;
use crate::settings::AppSettings;
use crate::timeline::{PlaybackScene, PlaybackTimeline, RtbSceneMetadata};
use crate::tracking::TrackingReporter;

use self::playback_log::{ActivePlaybackLog, PlaybackLogReporter, PlaybackLogScene};

#[derive(Clone)]
pub(crate) struct TrackingScene {
    pub metadata: RtbSceneMetadata,
    pub duration_millis: i64,
}

pub(crate) struct ActiveTrackingSession {
    pub scene_id: String,
    pub metadata: RtbSceneMetadata,
    pub started_at_millis: i64,
    pub duration_millis: i64,
    pub abort: tokio::task::AbortHandle,
}

/// 사이니지 재생 엔진.
///
/// 스케줄 동기화, 에셋 캐시, 재생 타이밍을 담당하는 중심 객체.
/// GPU/윈도우에 의존하지 않으므로 단위 테스트가 가능하다.
pub struct PlaybackEngine {
    /// 앱 설정 (deviceId, CMS URL, NTP 서버 등).
    pub(crate) settings: AppSettings,
    /// NTP/서버시간 보정 클럭. 모든 재생 판단의 기준 시간.
    pub(crate) clock: Arc<SignageClock>,
    /// CMS API 클라이언트 (revision / play_data 조회).
    pub(crate) cms: CmsApiClient,
    /// 로컬 에셋 캐시 저장소.
    pub(crate) assets: Arc<AssetStore>,
    /// 현재 엔진 상태 (상태머신).
    pub(crate) state: Arc<Mutex<EngineState>>,
    /// 현재 활성 타임라인.
    pub(crate) active_timeline: Arc<Mutex<Option<PlaybackTimeline>>>,
    /// 마지막으로 적용한 CMS revision. 같으면 play_data 재수신을 생략한다.
    pub(crate) last_revision: Arc<Mutex<Option<String>>>,
    /// 마지막으로 화면에 표출 완료된 scene_id (렌더 스레드 콜백으로 갱신).
    /// 재생 루프가 "지금 표출 중이어야 할 scene"과 비교해 누락을 복구한다.
    /// 동기 콜백에서 갱신되므로 std Mutex를 사용한다.
    pub(crate) last_switched_scene: Arc<StdMutex<Option<String>>>,
    /// scene_id → 재생 로그용 메타데이터 인덱스.
    pub(crate) playback_log_scenes: Arc<StdMutex<HashMap<String, PlaybackLogScene>>>,
    /// 현재 표출 중인 scene의 재생 로그 상태.
    pub(crate) active_playback_log: Arc<StdMutex<Option<ActivePlaybackLog>>>,
    /// 완료된 재생 로그를 배치 전송하는 큐.
    pub(crate) playback_log_reporter: PlaybackLogReporter,
    /// RTB 이벤트 URL 전용 비동기 큐.
    pub(crate) tracking_reporter: Arc<TrackingReporter>,
    /// scene_id → RTB tracking 메타데이터.
    pub(crate) tracking_scenes: Arc<StdMutex<HashMap<String, TrackingScene>>>,
    /// RTB prepare 실패 시 사용할 일반 편성 scene.
    pub(crate) fallback_scenes: Arc<StdMutex<HashMap<String, PlaybackScene>>>,
    /// 현재 revision에서 fallback으로 전환된 RTB 슬롯.
    pub(crate) failed_rtb_slots: Arc<StdMutex<HashSet<String>>>,
    /// 현재 표출 중인 RTB tracking 타이머.
    pub(crate) active_tracking: Arc<StdMutex<Option<ActiveTrackingSession>>>,
    /// 진단/UI용 이벤트 발신 채널.
    pub(crate) events: mpsc::UnboundedSender<EngineEvent>,
    /// 렌더 스레드로 보내는 전환 명령 채널.
    pub(crate) switch_tx: mpsc::UnboundedSender<SwitchCommand>,
    /// 재생 루프 세대 번호. 타임라인이 교체되면 증가시켜 구세대 루프를 종료시킨다.
    pub(crate) playback_generation: Arc<AtomicU64>,
    pub(crate) shutting_down: Arc<AtomicBool>,
    pub(crate) runtime_handle: Handle,
}

impl PlaybackEngine {
    /// 엔진을 생성한다. HTTP 클라이언트와 캐시 디렉터리를 초기화한다.
    pub fn new(
        settings: AppSettings,
        clock: Arc<SignageClock>,
        events: mpsc::UnboundedSender<EngineEvent>,
        switch_tx: mpsc::UnboundedSender<SwitchCommand>,
        runtime_handle: Handle,
    ) -> Result<Self> {
        let cms = CmsApiClient::new(&settings)?;
        let assets = Arc::new(AssetStore::new(&settings)?);
        Ok(Self {
            settings,
            clock,
            cms,
            assets,
            state: Arc::new(Mutex::new(EngineState::Idle)),
            active_timeline: Arc::new(Mutex::new(None)),
            last_revision: Arc::new(Mutex::new(None)),
            last_switched_scene: Arc::new(StdMutex::new(None)),
            playback_log_scenes: Arc::new(StdMutex::new(HashMap::new())),
            active_playback_log: Arc::new(StdMutex::new(None)),
            playback_log_reporter: PlaybackLogReporter::new(),
            tracking_reporter: Arc::new(TrackingReporter::new()?),
            tracking_scenes: Arc::new(StdMutex::new(HashMap::new())),
            fallback_scenes: Arc::new(StdMutex::new(HashMap::new())),
            failed_rtb_slots: Arc::new(StdMutex::new(HashSet::new())),
            active_tracking: Arc::new(StdMutex::new(None)),
            events,
            switch_tx,
            playback_generation: Arc::new(AtomicU64::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            runtime_handle,
        })
    }

    /// 엔진 백그라운드 작업을 시작한다.
    ///
    /// 1. 오프라인 캐시된 play_data가 있으면 즉시 재생 시작 (네트워크 불필요)
    /// 2. 주기 동기화 루프 시작 (NTP + CMS revision 폴링)
    /// 3. 5분 주기 캐시 정리 루프 시작
    pub async fn start(self: Arc<Self>) {
        self.playback_log_reporter.start(self.cms.clone());
        self.tracking_reporter.start();

        // 캐시 재생 시도 후 동기화 루프 진입.
        let engine = self.clone();
        tokio::spawn(async move {
            if let Err(err) = engine.load_cached_playback().await {
                warn!("cached playback unavailable: {err}");
            }
            engine.run_sync_loop().await;
        });

        // 보호 window 밖의 오래된 에셋을 주기적으로 정리.
        let cleanup = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                if let Err(err) = cleanup.cleanup_protected_assets().await {
                    warn!("cache cleanup failed: {err:#}");
                }
            }
        });
    }

    /// 상태를 변경하고, 실제로 바뀐 경우에만 이벤트를 발행한다.
    pub(crate) async fn set_state(&self, next: EngineState) {
        let mut state = self.state.lock().await;
        if *state != next {
            *state = next;
            let _ = self.events.send(EngineEvent::StateChanged(next));
        }
    }

    /// 보정 클럭 핸들을 반환한다 (렌더 스레드가 전환 타이밍 판단에 사용).
    pub fn clock(&self) -> Arc<SignageClock> {
        self.clock.clone()
    }

    /// 현재 설정을 반환한다.
    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    /// tracking 큐를 drain하고 재생 태스크가 새 작업을 만들지 않게 한다.
    pub async fn shutdown(&self) {
        use std::sync::atomic::Ordering;

        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.playback_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut active) = self.active_tracking.lock() {
            if let Some(active) = active.take() {
                active.abort.abort();
            }
        }
        self.tracking_reporter.shutdown().await;
    }
}
