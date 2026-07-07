//! 시간 보정 모듈.
//!
//! 사이니지 정시 재생의 기준 시간은 단말 시스템 시각이 아니라
//! NTP/서버시간으로 보정된 [`SignageClock`]이다.
//! 사용자가 단말 시각을 바꿔도 재생 판단이 흔들리지 않도록
//! monotonic clock(`Instant`) 기반 offset 방식을 사용한다.

pub mod mock;
pub mod signage;
pub mod sntp;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 클럭 신뢰도 등급. 높은 등급일수록 정시 재생 정확도가 보장된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClockConfidence {
    /// NTP 동기화 성공 직후 상태 (가장 신뢰도 높음).
    Synced,
    /// 마지막 동기화 이후 시간이 지났거나 동기화 실패 (이전 offset 유지 중).
    Stale,
    /// CMS `server_time`으로만 보정된 상태.
    ServerEstimated,
    /// 보정 정보 없음 — 단말 시스템 시각 사용 (신뢰도 낮음).
    DeviceClock,
    /// 사용 불가.
    Unusable,
}

/// 클럭의 현재 보정 상태 스냅샷 (진단/영속화에 사용).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockSnapshot {
    /// 보정 기준 epoch millis (신뢰도별로 의미가 다름 — 진단용).
    pub offset_millis: i64,
    /// 현재 신뢰도.
    pub confidence: ClockConfidence,
    /// 마지막 동기화로 얻은 서버 시각.
    pub last_sync_epoch_millis: Option<i64>,
    /// (예비) 동기화 시점의 monotonic 시각.
    pub anchor_instant_nanos: Option<u128>,
    /// 보정 출처 (NTP 서버 주소, "play_data.server_time" 등).
    pub source: String,
    /// offset 급변 등 경고 메시지.
    pub warning: Option<String>,
}

/// NTP 동기화 1회 시도의 결과.
#[derive(Debug, Clone)]
pub struct ClockSyncResult {
    pub snapshot: ClockSnapshot,
    /// 요청-응답 왕복 시간 (성공 시).
    pub round_trip_millis: Option<i64>,
    pub success: bool,
}

/// NTP 서버 응답 (파싱 완료 상태).
#[derive(Debug, Clone)]
pub struct NtpResult {
    /// 서버가 알려준 시각 (epoch millis).
    pub epoch_millis: i64,
    /// 요청-응답 왕복 시간.
    pub round_trip_millis: i64,
}

/// NTP 클라이언트 추상화. 실제 구현([`SntpClient`])과
/// 테스트용 mock을 교체할 수 있게 trait으로 분리한다.
#[async_trait]
pub trait NtpClient: Send + Sync {
    /// 지정한 서버에 시각을 요청한다.
    async fn request_time(&self, server: &str) -> anyhow::Result<NtpResult>;
}

/// 재생 로직이 사용하는 시간 소스 추상화.
/// 엔진/타임라인 테스트에서 MockClock을 주입할 수 있다.
pub trait Clock: Send + Sync {
    /// 보정된 현재 시각 (epoch millis).
    fn now_millis(&self) -> i64;
    /// 현재 보정 상태 스냅샷.
    fn snapshot(&self) -> ClockSnapshot;
}

pub use signage::SignageClock;
pub use sntp::SntpClient;
