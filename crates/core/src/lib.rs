//! OnePlayer core: NTP 보정 클럭, CMS 동기화, 타임라인, 에셋 캐시, 재생 엔진.
//!
//! 이 크레이트는 GPU/윈도우에 의존하지 않는 순수 로직만 담는다.
//! 렌더링은 `oneplayer-render`, 앱 조립은 `oneplayer` 바이너리가 담당한다.
//!
//! 모듈 개요:
//! - [`clock`]: NTP/서버시간 보정 시계 (모든 재생 판단의 기준 시간)
//! - [`cms`]: CMS API 클라이언트와 응답 DTO
//! - [`timeline`]: play_data → 시간순 scene 목록 변환
//! - [`cache`]: 에셋 다운로드/검증/LRU 정리
//! - [`engine`]: 동기화→다운로드→준비→전환 오케스트레이션 상태머신
//! - [`settings`]: config.toml 로드/저장
//! - [`config`]: 재생 정책 상수 (Android 0.4.0 정책과 1:1)

pub mod cache;
pub mod clock;
pub mod cms;
pub mod config;
pub mod engine;
pub mod settings;
pub mod timeline;
pub mod timing_log;

pub use cache::{AssetStore, CacheCleanupResult};
pub use clock::{Clock, ClockConfidence, ClockSnapshot, ClockSyncResult, SignageClock, SntpClient};
pub use cms::CmsApiClient;
pub use config::PolicyConfig;
pub use engine::{EngineEvent, EngineState, PlaybackEngine};
pub use settings::AppSettings;
pub use timeline::{
    AssetRef, LayoutDefinition, LayoutElement, PlaybackScene, PlaybackTimeline, TimelineBuilder,
};
