//! 동기화 로직: NTP 클럭 보정, CMS play_data 동기화, 에셋 선다운로드.
//!
//! 정책 (OnePlayer 0.4.0 계승):
//! - sync 주기는 기본 5분
//! - play_data 응답의 revision이 같으면 타임라인 재구성 없이 누락 에셋만 보충
//!   (단, 타임라인 확장 window 잔량이 임계값 미만이면 재구성 — 30분 window 소진 방지)
//! - NTP 실패 시 CMS `server_time`을 fallback으로 사용
//! - 마지막 성공 play_data는 로컬에 캐시해 오프라인 부팅에 사용

use std::sync::Arc;

use anyhow::Result;
use tracing::{error, info, warn};

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;

use crate::clock::{Clock, SntpClient};
use crate::cms::{CmsApiClient, PlaybackDataDto};
use crate::config::{DEFAULT_ZONE_ID, TIMELINE_REFRESH_THRESHOLD_MS};
use crate::timeline::{AssetRef, PlaybackTimeline, TimelineBuilder};

use super::state::{EngineEvent, EngineState};
use super::PlaybackEngine;

impl PlaybackEngine {
    /// 동기화 무한 루프. 설정된 주기(기본 5분)마다 [`Self::sync_once`]를 실행한다.
    /// 실패해도 루프는 끊기지 않고 다음 주기에 재시도한다.
    pub(crate) async fn run_sync_loop(self: Arc<Self>) {
        loop {
            if let Err(err) = self.sync_once().await {
                error!("sync failed: {err:#}");
                self.set_state(EngineState::Error).await;
                let _ = self.events.send(EngineEvent::Error(err.to_string()));
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                self.settings.sync_interval_ms() as u64,
            ))
            .await;
        }
    }

    /// 1회 동기화를 수행한다.
    ///
    /// 순서:
    /// 1. NTP로 클럭 보정 (실패 시 경고 후 계속)
    /// 2. CMS play_data 조회 (`device_id` + 오늘 `date`) + server_time으로 클럭 보조 보정
    /// 3. revision이 이전과 같으면 → 누락 에셋만 보충하고 종료
    /// 4. revision이 바뀌었으면 → 타임라인 재구성
    ///    → 선다운로드(blocking) + 나머지 백그라운드 다운로드 → 재생 루프 재시작
    pub(crate) async fn sync_once(self: &Arc<Self>) -> Result<()> {
        self.set_state(EngineState::Syncing).await;
        let _ = self.events.send(EngineEvent::Status("syncing".into()));

        // 1. NTP 클럭 보정. 실패해도 마지막 offset을 유지하므로 계속 진행한다.
        let ntp = SntpClient::default();
        let ntp_result = self
            .clock
            .sync_with_ntp(&ntp, &self.settings.ntp_server)
            .await;
        if !ntp_result.success {
            warn!("NTP sync failed, using stale/server fallback if available");
        }

        // 2. play_data 조회. server_time이 있으면 클럭 보조 보정에 사용한다.
        let date = playback_date(self.clock.now_millis());
        let data = self
            .cms
            .get_playback_data(&self.settings.device_id, &date)
            .await?;
        if let Some(server_time) = &data.server_time {
            self.clock.apply_server_time(server_time);
        }

        // 3. revision이 같으면 원칙적으로 타임라인 교체 불필요 — 누락 에셋만 보충.
        //    단, 다중 item slot 확장 window(미래 30분) 잔량이 임계값 미만이거나
        //    날짜가 바뀌었으면 현재 시각 기준으로 재구성한다 (스케줄 소진 방지).
        let last = self.last_revision.lock().await.clone();
        if last.as_deref() == Some(data.revision.as_str()) {
            self.download_missing_preload_assets().await?;
            if self.timeline_needs_refresh(&data).await {
                info!(revision = %data.revision, "timeline window low, rebuilding");
                let timeline = TimelineBuilder::build(&data, self.clock.now_millis());
                // 동일 revision 재구성이므로 오프라인 캐시는 다시 쓰지 않는다.
                self.apply_timeline(&timeline).await?;
            }
            return Ok(());
        }

        // 4. revision 변경 → 타임라인 재구성.
        let timeline = TimelineBuilder::build(&data, self.clock.now_millis());
        // 오프라인 부팅용으로 마지막 성공 응답을 저장한다.
        CmsApiClient::save_playback_cache(&self.settings.playback_cache_path(), &data)?;
        self.apply_timeline(&timeline).await?;
        Ok(())
    }

    /// 활성 타임라인의 확장 window가 소진 임박인지 판단한다.
    ///
    /// 기준:
    /// - 활성 타임라인이 없으면 → 재구성 필요
    /// - 스케줄 날짜가 응답과 다르면 (자정 경과) → 재구성 필요
    /// - 마지막 scene 시작까지 남은 시간이 임계값(2분) 미만이면 → 재구성 필요
    async fn timeline_needs_refresh(&self, data: &PlaybackDataDto) -> bool {
        let now = self.clock.now_millis();
        let guard = self.active_timeline.lock().await;
        match guard.as_ref() {
            None => true,
            Some(timeline) => {
                timeline.date != data.date
                    || timeline
                        .scenes
                        .last()
                        .map_or(true, |s| s.start_time_millis - now < TIMELINE_REFRESH_THRESHOLD_MS)
            }
        }
    }

    /// 타임라인을 활성화한다: preload window 에셋 blocking 준비
    /// → 나머지 백그라운드 다운로드 → 활성 타임라인 교체 → 재생 루프 재시작.
    async fn apply_timeline(self: &Arc<Self>, timeline: &PlaybackTimeline) -> Result<()> {
        // 표출 임박(preload window) 에셋은 blocking으로 준비한다.
        self.set_state(EngineState::Downloading).await;
        let blocking = self.preload_window_assets(timeline);
        self.assets.ensure_assets(&blocking).await?;

        // 나머지 에셋은 재생을 막지 않도록 백그라운드에서 받는다.
        self.spawn_background_downloads(timeline, &blocking);

        // 새 타임라인을 활성화하고 재생 루프를 재시작한다.
        {
            *self.active_timeline.lock().await = Some(timeline.clone());
            *self.last_revision.lock().await = Some(timeline.revision.clone());
        }
        let _ = self.events.send(EngineEvent::TimelineUpdated {
            revision: timeline.revision.clone(),
            scene_count: timeline.scenes.len(),
        });
        self.spawn_playback_loop(timeline.clone());
        Ok(())
    }

    /// 오프라인 캐시된 play_data로 재생을 시작한다 (cold start 가속).
    ///
    /// 조건: 캐시의 deviceId가 현재 설정과 일치하고,
    /// preload window 안의 모든 에셋이 이미 로컬에 준비되어 있어야 한다.
    /// 조건 미달이면 에러를 반환하고 일반 동기화 경로를 따른다.
    pub(crate) async fn load_cached_playback(self: &Arc<Self>) -> Result<()> {
        let path = self.settings.playback_cache_path();
        let data = CmsApiClient::load_playback_cache(&path)?
            .ok_or_else(|| anyhow::anyhow!("no cached playback"))?;
        // 다른 단말의 캐시는 사용하지 않는다 (deviceId 검증).
        if data.device_id != self.settings.device_id {
            anyhow::bail!("cached device mismatch");
        }

        let timeline = TimelineBuilder::build(&data, self.clock.now_millis());
        // 필요한 에셋이 하나라도 없으면 캐시 재생을 포기한다
        // (표출 시점 다운로드 금지 정책).
        let blocking = self.preload_window_assets(&timeline);
        self.assets.verify_all_ready(&blocking)?;

        {
            *self.active_timeline.lock().await = Some(timeline.clone());
            *self.last_revision.lock().await = Some(timeline.revision.clone());
        }
        let _ = self
            .events
            .send(EngineEvent::Status("cached playback loaded".into()));
        self.spawn_playback_loop(timeline);
        Ok(())
    }

    /// revision 변경이 없을 때, preload window에 새로 들어온 에셋 중
    /// 아직 로컬에 없는 것만 다운로드한다.
    pub(crate) async fn download_missing_preload_assets(&self) -> Result<()> {
        let timeline = self.active_timeline.lock().await.clone();
        let Some(timeline) = timeline else {
            return Ok(());
        };
        let missing: Vec<_> = self
            .preload_window_assets(&timeline)
            .into_iter()
            .filter(|a| !self.assets.is_ready(a))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        self.set_state(EngineState::Downloading).await;
        self.assets.ensure_assets(&missing).await?;
        Ok(())
    }

    /// 표출 전에 반드시(blocking) 준비해야 할 에셋 목록을 계산한다.
    ///
    /// 대상: 현재 scene + 다음 scene + preload window(기본 5분) 안의 모든 scene.
    /// 중복은 cache_key 기준으로 제거한다.
    pub(crate) fn preload_window_assets(&self, timeline: &PlaybackTimeline) -> Vec<AssetRef> {
        let now = self.clock.now_millis();
        let window_end = now + self.settings.asset_preload_window_ms();

        let mut refs: Vec<_> = timeline
            .scenes_in_window(now, window_end)
            .into_iter()
            .flat_map(|s| s.asset_refs.clone())
            .collect();
        if let Some(current) = timeline.current_scene(now) {
            refs.extend(current.asset_refs.clone());
        }
        if let Some(next) = timeline.next_scene(now) {
            refs.extend(next.asset_refs.clone());
        }

        refs.sort_by(|a, b| a.cache_key().cmp(&b.cache_key()));
        refs.dedup_by(|a, b| a.cache_key() == b.cache_key());
        refs
    }

    /// preload 대상이 아닌 나머지 타임라인 에셋을 백그라운드 태스크로 다운로드한다.
    /// 실패해도 재생을 막지 않는다 (경고 로그만 남김).
    fn spawn_background_downloads(&self, timeline: &PlaybackTimeline, blocking: &[AssetRef]) {
        let background: Vec<_> = timeline
            .all_asset_refs()
            .into_iter()
            .filter(|a| !blocking.iter().any(|b| b.cache_key() == a.cache_key()))
            .collect();
        if background.is_empty() {
            return;
        }
        let assets = self.assets.clone();
        tokio::spawn(async move {
            if let Err(err) = assets.ensure_assets(&background).await {
                warn!("background asset download failed: {err:#}");
            }
        });
    }
}

/// 보정 클럭 기준 스케줄 조회용 날짜 (`YYYY-MM-DD`).
fn playback_date(now_millis: i64) -> String {
    let tz: Tz = DEFAULT_ZONE_ID.parse().unwrap_or(chrono_tz::Asia::Seoul);
    Utc.timestamp_millis_opt(now_millis)
        .single()
        .unwrap_or_else(Utc::now)
        .with_timezone(&tz)
        .format("%Y-%m-%d")
        .to_string()
}
