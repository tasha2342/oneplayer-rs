//! 엔진 상태머신과 이벤트 타입 정의.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::timeline::PlaybackScene;

/// 재생 엔진 상태.
///
/// 정상 흐름: `Idle → Syncing → Downloading → Preparing → Ready → Playing`
/// 오류 발생 시 `Error`로 전이 후 다음 sync 주기에 복구를 시도한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// 시작 전 초기 상태.
    Idle,
    /// NTP/CMS 동기화 중.
    Syncing,
    /// 에셋 다운로드 중.
    Downloading,
    /// scene 준비(디코드/텍스처 업로드) 진행 중.
    Preparing,
    /// 다음 전환 대기 중 (준비 완료).
    Ready,
    /// 장면 표출 중.
    Playing,
    /// 오류 상태 (다음 sync에서 재시도).
    Error,
}

/// 엔진이 발행하는 이벤트. 진단 로그와 v2 디버그 overlay의 데이터 소스가 된다.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 상태머신 전이.
    StateChanged(EngineState),
    /// 새 타임라인 적용 완료.
    TimelineUpdated {
        revision: String,
        scene_count: usize,
    },
    /// scene 준비 시작 (T-12초 window 진입).
    ScenePrepared {
        scene_id: String,
        target_time_millis: i64,
    },
    /// 레이어 전환 완료. `delay_millis = actual - target` (목표: ±100ms).
    SceneSwitched {
        scene_id: String,
        target_time_millis: i64,
        actual_time_millis: i64,
        delay_millis: i64,
    },
    /// 전환 실패 (prepare 실패, 에셋 누락 등).
    SwitchFailed { scene_id: String, reason: String },
    /// 복구 불가능한 오류.
    Error(String),
    /// 사람이 읽는 상태 메시지 (로딩 overlay 용).
    Status(String),
}

/// 엔진 → 렌더 스레드로 전달되는 전환 명령.
///
/// 렌더 스레드는 이 명령을 받아 scene을 prepare하고
/// `target_time_millis`에 hidden layer를 전환한다.
pub struct SwitchCommand {
    /// 표출할 장면.
    pub scene: PlaybackScene,
    /// 전환 목표 시각 (SignageClock 기준 epoch millis).
    pub target_time_millis: i64,
    /// cache_key → 로컬 파일 경로 (prepare 단계에서 파일 접근에 사용).
    pub local_files: HashMap<String, PathBuf>,
}
