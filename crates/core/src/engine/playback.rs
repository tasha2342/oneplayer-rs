//! 재생 루프: 다음 scene 선정 → prepare 예약 → 전환 명령 발송 → 결과 수신.
//!
//! 정책 (OnePlayer 0.4.0 계승):
//! - scene prepare window: 12초 (T-12초에 준비 시작)
//! - 영상 preroll window: 8초 (T-8초에 preroll 시작)
//! - 표출 시점 다운로드 금지 — 에셋 미준비 scene은 전환하지 않고 재시도
//! - 실제 정밀 전환(T-1초 이후 frame loop)은 렌더 스레드가 담당

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::clock::Clock;
use crate::config::{
    FILE_CACHE_WARM_WINDOW_MS, MIN_PLAYBACK_LOOP_DELAY_MS, NO_SCENE_RETRY_MS,
    SCENE_PREPARE_WINDOW_MS, VIDEO_PREROLL_WINDOW_MS,
};
use crate::timeline::{PlaybackScene, PlaybackTimeline};

use super::state::{EngineEvent, EngineState, SwitchCommand};
use super::PlaybackEngine;

impl PlaybackEngine {
    /// 새 세대(generation)의 재생 루프를 백그라운드 태스크로 시작한다.
    ///
    /// 세대 번호를 증가시켜 이전 타임라인의 루프가 스스로 종료되게 한다
    /// (타임라인 교체 시 구세대 루프의 전환 명령이 새 화면을 덮지 않도록).
    pub(crate) fn spawn_playback_loop(self: &Arc<Self>, timeline: PlaybackTimeline) {
        let generation = self.playback_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let engine = self.clone();
        tokio::spawn(async move {
            engine.playback_loop(timeline, generation).await;
        });
    }

    /// 재생 루프 본체. 다음 scene을 골라 prepare/전환을 예약하는 것을 반복한다.
    ///
    /// 루프 1회의 흐름:
    /// 1. 세대 확인 — 타임라인이 교체됐으면 종료
    /// 2. 다음 scene 선정 — 없으면 5초 후 재시도
    /// 3. T-12초(prepare window) 전이면 그 시각까지 대기
    /// 4. 에셋 준비 확인 — 미준비면 5초 후 재시도 (표출 시점 다운로드 금지)
    /// 5. 영상 scene이면 T-8초(preroll window)까지 대기
    /// 6. 렌더 스레드에 전환 명령 발송
    /// 7. scene 종료 시각까지 대기 후 다음 반복
    async fn playback_loop(self: Arc<Self>, timeline: PlaybackTimeline, generation: u64) {
        loop {
            // 1. 구세대 루프면 종료.
            if self.playback_generation.load(Ordering::SeqCst) != generation {
                break;
            }

            // 2. 다음 표출 scene 선정.
            let now = self.clock.now_millis();
            let Some(next) = timeline.next_scene(now) else {
                self.set_state(EngineState::Ready).await;
                let _ = self
                    .events
                    .send(EngineEvent::Status("no upcoming scene".into()));
                sleep_ms(NO_SCENE_RETRY_MS).await;
                continue;
            };

            // 3. 아직 prepare window(T-12초) 전이면 진입 시각까지 대기.
            let prepare_at = next.start_time_millis - SCENE_PREPARE_WINDOW_MS;
            if now < prepare_at {
                sleep_ms((prepare_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS)).await;
                continue;
            }

            // 4. 에셋이 로컬에 없으면 전환하지 않는다 (표출 시점 다운로드 금지).
            if !self.scene_assets_ready(next) {
                warn!(scene_id = %next.scene_id, "scene assets missing, retrying");
                sleep_ms(NO_SCENE_RETRY_MS).await;
                continue;
            }

            // prepare 시작을 알린다 (실제 디코드/텍스처 업로드는 렌더 스레드).
            self.set_state(EngineState::Preparing).await;
            let local_files = self.local_files.lock().await.clone();
            let _ = self.events.send(EngineEvent::ScenePrepared {
                scene_id: next.scene_id.clone(),
                target_time_millis: next.start_time_millis,
            });

            // 5. 영상 scene은 preroll window(T-8초)에 맞춰 전환 명령을 보낸다.
            //    너무 일찍 보내면 디코더 점유 시간이 길어지기 때문.
            if next.has_video() {
                let preroll_at = next.start_time_millis - VIDEO_PREROLL_WINDOW_MS;
                if now < preroll_at {
                    sleep_ms((preroll_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS)).await;
                }
            }

            // 6. 렌더 스레드로 전환 명령 발송. 수신자가 없으면 앱 종료로 보고 루프 탈출.
            self.set_state(EngineState::Ready).await;
            let cmd = SwitchCommand {
                scene: next.clone(),
                target_time_millis: next.start_time_millis,
                local_files,
            };
            if self.switch_tx.send(cmd).is_err() {
                break;
            }

            // 7. scene 종료 직전까지 대기했다가 다음 scene 처리로 넘어간다.
            let wait_until = next.end_time_millis - MIN_PLAYBACK_LOOP_DELAY_MS;
            let now = self.clock.now_millis();
            if wait_until > now {
                sleep_ms((wait_until - now).max(MIN_PLAYBACK_LOOP_DELAY_MS)).await;
            }
            self.set_state(EngineState::Playing).await;
        }
    }

    /// scene의 모든 에셋이 로컬 캐시에 준비되어 있는지 확인한다.
    fn scene_assets_ready(&self, scene: &PlaybackScene) -> bool {
        scene.asset_refs.iter().all(|a| self.assets.is_ready(a))
    }

    /// 렌더 스레드가 레이어 전환을 완료했을 때 호출하는 콜백.
    /// `delay = actual - target`을 로그와 이벤트로 남긴다 (목표: ±100ms).
    pub fn on_scene_switched(
        &self,
        scene_id: &str,
        target_time_millis: i64,
        actual_time_millis: i64,
    ) {
        let delay = actual_time_millis - target_time_millis;
        info!(
            scene_id,
            target_time_millis,
            actual_time_millis,
            delay_millis = delay,
            "scene switched"
        );
        let _ = self.events.send(EngineEvent::SceneSwitched {
            scene_id: scene_id.to_string(),
            target_time_millis,
            actual_time_millis,
            delay_millis: delay,
        });
    }

    /// 렌더 스레드가 전환에 실패했을 때 호출하는 콜백.
    /// fallback 정책상 실패 시 현재 scene이 유지되므로 여기서는 기록만 한다.
    pub fn on_switch_failed(&self, scene_id: &str, reason: &str) {
        warn!(scene_id, reason, "switch failed");
        let _ = self.events.send(EngineEvent::SwitchFailed {
            scene_id: scene_id.to_string(),
            reason: reason.to_string(),
        });
    }

    /// 보호 대상(현재/다음/warm window 20분 내 scene의 에셋)을 제외하고
    /// 캐시를 정리한다. 5분 주기로 호출된다.
    pub async fn cleanup_protected_assets(&self) -> Result<()> {
        let timeline = self.active_timeline.lock().await.clone();
        let now = self.clock.now_millis();

        // 삭제하면 안 되는 cache_key 집합을 수집한다.
        let mut protected = HashSet::new();
        if let Some(timeline) = timeline {
            let mut protect_scene = |scene: &PlaybackScene| {
                for a in &scene.asset_refs {
                    protected.insert(a.cache_key());
                }
            };
            if let Some(current) = timeline.current_scene(now) {
                protect_scene(current);
            }
            if let Some(next) = timeline.next_scene(now) {
                protect_scene(next);
            }
            for scene in timeline.scenes_in_window(now, now + FILE_CACHE_WARM_WINDOW_MS) {
                protect_scene(scene);
            }
        }

        self.assets.cleanup_cache(&protected, now).await?;
        Ok(())
    }
}

/// 밀리초 단위 tokio sleep 헬퍼.
async fn sleep_ms(ms: i64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms.max(0) as u64)).await;
}
