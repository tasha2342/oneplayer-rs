//! 재생 정책 상수 모음.
//!
//! OnePlayer 0.4.0 Android 정책 문서와 1:1로 대응한다.
//! 정책 수치를 바꿀 때는 이 파일만 수정하면 되도록 다른 모듈에서
//! 숫자를 직접 쓰지 않는다.

/// NTP/스케줄 동기화 주기 (5분).
pub const SYNC_INTERVAL_MS: i64 = 5 * 60 * 1_000;

/// scene prepare window — 표출 T-12초에 준비를 시작한다.
pub const SCENE_PREPARE_WINDOW_MS: i64 = 12 * 1_000;

/// 앱 시작 직후 warm-up window (60초).
pub const STARTUP_WARMUP_WINDOW_MS: i64 = 60 * 1_000;

/// 재생 루프 최소 대기 시간 — busy loop 방지.
pub const MIN_PLAYBACK_LOOP_DELAY_MS: i64 = 50;

/// 타임라인 재생성 판단 임계값 (2분).
pub const TIMELINE_REFRESH_THRESHOLD_MS: i64 = 2 * 60 * 1_000;

/// 에셋 선다운로드 window — 표출 5분 전까지 blocking 다운로드 완료.
pub const ASSET_PRELOAD_WINDOW_MS: i64 = 5 * 60 * 1_000;

/// 파일 캐시 보호 window — 향후 20분 내 재생 예정 에셋은 삭제하지 않는다.
pub const FILE_CACHE_WARM_WINDOW_MS: i64 = 20 * 60 * 1_000;

/// 정밀 전환 window — T-1초부터는 sleep 대신 렌더 프레임 루프로 시각을 검사한다.
pub const PRECISE_WINDOW_MS: i64 = 1_000;

/// 영상 preroll window — T-8초에 muted 디코드를 시작한다.
pub const VIDEO_PREROLL_WINDOW_MS: i64 = 8 * 1_000;

/// 영상 전환 예약 여유 시간 (700ms).
pub const VIDEO_SWITCH_RESERVE_MS: i64 = 700;

/// 영상 첫 프레임 대기 한도 — target 이후 최대 2초까지 현재 화면 유지.
pub const VIDEO_FIRST_FRAME_WAIT_MS: i64 = 2_000;

/// 클럭 offset 급변 경고 임계값 (1초).
pub const LARGE_OFFSET_CHANGE_MS: i64 = 1_000;

/// 에셋 캐시 총량 상한 기본값 (1GB).
pub const DEFAULT_MAX_CACHE_SIZE_BYTES: u64 = 1_024 * 1_024 * 1_024;

/// `.part` 임시 파일 방치 한도 (30분) — 넘으면 정리 대상.
pub const STALE_PART_MAX_AGE_MS: i64 = 30 * 60 * 1_000;

/// 캐시 삭제 grace — 마지막 사용 후 5분간은 삭제하지 않는다.
pub const SCHEDULE_END_GRACE_MS: i64 = 5 * 60 * 1_000;

/// 타임라인 확장 과거 window (2분).
pub const TIMELINE_PAST_WINDOW_MS: i64 = 2 * 60 * 1_000;

/// 타임라인 확장 미래 window (30분).
pub const TIMELINE_FUTURE_WINDOW_MS: i64 = 30 * 60 * 1_000;

/// slot당 최대 확장 scene 수 (메모리 보호).
pub const MAX_EXPANDED_SCENES: usize = 2_000;

/// item duration 미지정 시 기본값 (15초).
pub const DEFAULT_ITEM_DURATION_SECONDS: i64 = 15;

/// 1초 (ms).
pub const ONE_SECOND_MS: i64 = 1_000;

/// 1일 (ms) — 자정을 넘는 slot 계산에 사용.
pub const DAY_MS: i64 = 24 * 60 * 60 * ONE_SECOND_MS;

/// slot 시각 파싱 기본 타임존.
pub const DEFAULT_ZONE_ID: &str = "Asia/Seoul";

/// 표출할 scene이 없을 때 재시도 간격 (5초).
pub const NO_SCENE_RETRY_MS: i64 = 5_000;

/// 런타임에 조정 가능한 정책 값 묶음 (테스트/튜닝용).
#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub sync_interval_ms: i64,
    pub scene_prepare_window_ms: i64,
    pub asset_preload_window_ms: i64,
    pub max_cache_size_bytes: u64,
    pub precise_window_ms: i64,
    pub video_preroll_window_ms: i64,
}

impl Default for PolicyConfig {
    /// 정책 문서 기본값으로 초기화한다.
    fn default() -> Self {
        Self {
            sync_interval_ms: SYNC_INTERVAL_MS,
            scene_prepare_window_ms: SCENE_PREPARE_WINDOW_MS,
            asset_preload_window_ms: ASSET_PRELOAD_WINDOW_MS,
            max_cache_size_bytes: DEFAULT_MAX_CACHE_SIZE_BYTES,
            precise_window_ms: PRECISE_WINDOW_MS,
            video_preroll_window_ms: VIDEO_PREROLL_WINDOW_MS,
        }
    }
}
