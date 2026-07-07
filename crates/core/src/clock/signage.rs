//! NTP/서버시간 보정 클럭 [`SignageClock`] 구현.
//!
//! 동작 원리 (Android의 `elapsedRealtime + offset` 방식과 동일):
//! - 동기화 시점의 서버 시각(`epoch_at_base`)과 monotonic 시각(`base_instant`)을 기록
//! - `now_millis() = epoch_at_base + base_instant.elapsed()`
//! - 시스템 시각이 바뀌어도 monotonic clock은 영향받지 않으므로 안정적이다.
//!
//! 신뢰도 우선순위: NTP(`Synced`) > 서버시간(`ServerEstimated`) > 이전값(`Stale`)
//! > 시스템시각(`DeviceClock`). NTP 실패 시 마지막 보정값을 유지한다.

use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::LARGE_OFFSET_CHANGE_MS;
use crate::settings::AppSettings;

use super::{Clock, ClockConfidence, ClockSnapshot, ClockSyncResult, NtpClient, NtpResult};

/// 클럭 상태 영속화 포맷 (JSON 파일).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedClockState {
    snapshot: ClockSnapshot,
}

/// NTP/서버시간 보정 클럭. 앱 전체의 재생 판단은 이 클럭만 사용한다.
pub struct SignageClock {
    /// 시각 계산의 핵심 상태 (epoch 기준점 + monotonic 기준점).
    inner: Arc<RwLock<ClockInner>>,
    /// 진단/영속화용 스냅샷 (신뢰도, 출처, 경고 등).
    snapshot: Arc<RwLock<ClockSnapshot>>,
    /// 스냅샷 저장 파일 경로 (테스트에서는 None).
    state_path: Option<std::path::PathBuf>,
}

/// 시각 계산 상태.
struct ClockInner {
    /// `base_instant` 시점의 보정된 epoch millis.
    epoch_at_base: i64,
    /// monotonic 기준점. 이후 경과 시간을 더해 현재 시각을 계산한다.
    base_instant: Instant,
}

impl SignageClock {
    /// 설정 기반으로 클럭을 생성한다.
    /// 이전 실행에서 저장한 스냅샷이 있으면 `Stale` 등급으로 복원한다.
    pub fn new(settings: &AppSettings) -> Self {
        let state_path = settings.clock_state_path();
        let snapshot = load_persisted(&state_path).unwrap_or_else(device_clock_snapshot);
        Self {
            inner: Arc::new(RwLock::new(ClockInner {
                // 동기화 전까지는 시스템 시각을 기준점으로 사용한다.
                epoch_at_base: chrono::Utc::now().timestamp_millis(),
                base_instant: Instant::now(),
            })),
            snapshot: Arc::new(RwLock::new(snapshot)),
            state_path: Some(state_path),
        }
    }

    /// 테스트용: 지정한 스냅샷으로 클럭을 생성한다 (파일 영속화 없음).
    pub fn with_snapshot(snapshot: ClockSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ClockInner {
                epoch_at_base: chrono::Utc::now().timestamp_millis(),
                base_instant: Instant::now(),
            })),
            snapshot: Arc::new(RwLock::new(snapshot)),
            state_path: None,
        }
    }

    /// NTP 서버와 동기화한다.
    ///
    /// 서버 응답 시각에 왕복 시간의 절반(midpoint)을 더해 전송 지연을 보정한다.
    /// 이전 보정값과 1초 이상 차이 나면 경고를 남긴다 (즉시 반영은 하되 진단 기록).
    /// 실패 시 이전 offset을 유지하고 `Stale`로 강등한다.
    pub async fn sync_with_ntp<N: NtpClient + ?Sized>(
        &self,
        client: &N,
        server: &str,
    ) -> ClockSyncResult {
        let started = Instant::now();
        match client.request_time(server).await {
            Ok(result) => self.apply_ntp_result(server, result, started),
            Err(err) => {
                // 실패해도 마지막 보정값은 유지된다 (Stale 강등만).
                let stale = self.mark_stale(&format!("NTP failed: {err}"));
                ClockSyncResult {
                    snapshot: stale,
                    round_trip_millis: None,
                    success: false,
                }
            }
        }
    }

    /// NTP 성공 응답을 클럭에 반영한다.
    fn apply_ntp_result(
        &self,
        server: &str,
        result: NtpResult,
        started: Instant,
    ) -> ClockSyncResult {
        // 왕복 시간의 절반을 더해 "응답이 도착한 순간"의 서버 시각을 추정한다.
        let midpoint = started.elapsed().as_millis() as i64 / 2;
        let corrected_epoch = result.epoch_millis + midpoint;

        // 보정값이 급변(>1초)하면 경고 — 네트워크 이상 또는 시각 조작 신호일 수 있다.
        let previous = self.snapshot();
        let previous_now = self.now_millis();
        let warning = if previous.confidence != ClockConfidence::Unusable
            && previous.confidence != ClockConfidence::DeviceClock
            && (previous_now - corrected_epoch).abs() > LARGE_OFFSET_CHANGE_MS
        {
            Some(format!(
                "Large clock offset change: {previous_now} -> {corrected_epoch}"
            ))
        } else {
            None
        };
        if let Some(ref msg) = warning {
            warn!(%msg, "clock offset jump");
        }

        self.set_epoch(corrected_epoch);
        let snapshot = ClockSnapshot {
            offset_millis: corrected_epoch,
            confidence: ClockConfidence::Synced,
            last_sync_epoch_millis: Some(result.epoch_millis),
            anchor_instant_nanos: None,
            source: server.to_string(),
            warning,
        };
        self.store_snapshot(&snapshot);
        ClockSyncResult {
            snapshot,
            round_trip_millis: Some(result.round_trip_millis),
            success: true,
        }
    }

    /// CMS 응답의 `server_time`(RFC3339)으로 클럭을 보조 보정한다.
    /// NTP보다 신뢰도가 낮은 `ServerEstimated` 등급으로 기록된다.
    pub fn apply_server_time(&self, server_time_iso: &str) {
        let Ok(dt) = DateTime::parse_from_rfc3339(server_time_iso) else {
            return; // 파싱 실패하면 기존 보정을 유지한다.
        };
        let server_millis = dt.with_timezone(&Utc).timestamp_millis();
        self.set_epoch(server_millis);
        self.store_snapshot(&ClockSnapshot {
            offset_millis: server_millis,
            confidence: ClockConfidence::ServerEstimated,
            last_sync_epoch_millis: Some(server_millis),
            anchor_instant_nanos: None,
            source: "play_data.server_time".into(),
            warning: None,
        });
    }

    /// 동기화 실패 시 신뢰도를 `Stale`로 강등한다 (보정값은 유지).
    /// 보정 이력이 아예 없으면 `DeviceClock`으로 전환한다.
    pub fn mark_stale(&self, reason: &str) -> ClockSnapshot {
        let current = self.snapshot();
        let next = match current.confidence {
            ClockConfidence::Synced
            | ClockConfidence::ServerEstimated
            | ClockConfidence::Stale => ClockSnapshot {
                confidence: ClockConfidence::Stale,
                warning: Some(reason.to_string()),
                ..current
            },
            ClockConfidence::DeviceClock | ClockConfidence::Unusable => {
                device_clock_snapshot_with_warning(reason)
            }
        };
        self.store_snapshot(&next);
        next
    }

    /// 시각 기준점을 갱신한다: "지금 monotonic 시각 = epoch_millis".
    fn set_epoch(&self, epoch_millis: i64) {
        let mut inner = self.inner.write().expect("clock lock poisoned");
        inner.epoch_at_base = epoch_millis;
        inner.base_instant = Instant::now();
    }

    /// 스냅샷을 메모리와 파일에 저장한다.
    fn store_snapshot(&self, snapshot: &ClockSnapshot) {
        *self.snapshot.write().expect("clock lock poisoned") = snapshot.clone();
        if let Some(path) = &self.state_path {
            let _ = persist_snapshot(path, snapshot);
        }
    }
}

impl Clock for SignageClock {
    /// 보정된 현재 시각을 반환한다.
    ///
    /// 보정 이력이 없으면(`DeviceClock`/`Unusable`) 시스템 시각으로 폴백한다.
    fn now_millis(&self) -> i64 {
        let snap = self.snapshot.read().expect("clock lock poisoned");
        match snap.confidence {
            ClockConfidence::DeviceClock | ClockConfidence::Unusable => {
                chrono::Utc::now().timestamp_millis()
            }
            _ => {
                let inner = self.inner.read().expect("clock lock poisoned");
                inner.epoch_at_base + inner.base_instant.elapsed().as_millis() as i64
            }
        }
    }

    /// 현재 보정 상태 스냅샷을 반환한다.
    fn snapshot(&self) -> ClockSnapshot {
        self.snapshot.read().expect("clock lock poisoned").clone()
    }
}

/// 보정 이력이 없을 때의 기본 스냅샷을 만든다.
fn device_clock_snapshot() -> ClockSnapshot {
    device_clock_snapshot_with_warning("No persisted clock offset")
}

/// 경고 메시지를 포함한 `DeviceClock` 등급 스냅샷을 만든다.
fn device_clock_snapshot_with_warning(reason: &str) -> ClockSnapshot {
    ClockSnapshot {
        offset_millis: chrono::Utc::now().timestamp_millis(),
        confidence: ClockConfidence::DeviceClock,
        last_sync_epoch_millis: None,
        anchor_instant_nanos: None,
        source: "device_clock".into(),
        warning: Some(reason.to_string()),
    }
}

/// 저장된 클럭 스냅샷을 읽는다.
/// 프로세스가 재시작되면 monotonic 기준점이 무효화되므로
/// `Synced`였던 스냅샷은 `Stale`로 강등해 복원한다.
fn load_persisted(path: &std::path::Path) -> Option<ClockSnapshot> {
    let raw = std::fs::read_to_string(path).ok()?;
    let state: PersistedClockState = serde_json::from_str(&raw).ok()?;
    let mut snapshot = state.snapshot;
    if snapshot.confidence == ClockConfidence::Synced {
        snapshot.confidence = ClockConfidence::Stale;
        snapshot.warning = Some("Restored persisted clock offset".into());
    }
    Some(snapshot)
}

/// 클럭 스냅샷을 JSON 파일로 저장한다.
fn persist_snapshot(path: &std::path::Path, snapshot: &ClockSnapshot) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let state = PersistedClockState {
        snapshot: snapshot.clone(),
    };
    std::fs::write(path, serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

/// 테스트용 고정 시각 클럭.
pub struct MockClock {
    now_millis: i64,
    snapshot: ClockSnapshot,
}

impl MockClock {
    /// 지정한 epoch millis로 고정된 클럭을 만든다.
    pub fn at(epoch_millis: i64) -> Self {
        Self {
            now_millis: epoch_millis,
            snapshot: ClockSnapshot {
                offset_millis: epoch_millis,
                confidence: ClockConfidence::Synced,
                last_sync_epoch_millis: Some(epoch_millis),
                anchor_instant_nanos: None,
                source: "mock".into(),
                warning: None,
            },
        }
    }

    /// 시각을 앞으로 이동시킨다 (테스트 시나리오 진행용).
    pub fn advance(&mut self, delta_ms: i64) {
        self.now_millis += delta_ms;
    }
}

impl Clock for MockClock {
    fn now_millis(&self) -> i64 {
        self.now_millis
    }

    fn snapshot(&self) -> ClockSnapshot {
        self.snapshot.clone()
    }
}

/// 테스트용 NTP mock 구현 모음.
pub mod mock {
    use super::*;

    /// 항상 고정된 결과를 반환하는 NTP 클라이언트.
    pub struct FixedNtpClient {
        pub result: NtpResult,
    }

    #[async_trait]
    impl NtpClient for FixedNtpClient {
        async fn request_time(&self, _server: &str) -> anyhow::Result<NtpResult> {
            Ok(self.result.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::FixedNtpClient;
    use super::*;

    /// NTP 동기화 성공 시 신뢰도가 `Synced`로 올라가는지 확인한다.
    #[tokio::test]
    async fn ntp_sync_updates_offset() {
        let clock = SignageClock::with_snapshot(ClockSnapshot {
            offset_millis: 0,
            confidence: ClockConfidence::Stale,
            last_sync_epoch_millis: None,
            anchor_instant_nanos: None,
            source: "test".into(),
            warning: None,
        });
        let client = FixedNtpClient {
            result: NtpResult {
                epoch_millis: 1_700_000_000_000,
                round_trip_millis: 10,
            },
        };
        let result = clock.sync_with_ntp(&client, "time.example.com").await;
        assert!(result.success);
        assert_eq!(result.snapshot.confidence, ClockConfidence::Synced);
    }
}
