//! 재생 루프: 다음 scene 선정 → prepare 예약 → 전환 명령 발송 → 결과 수신.
//!
//! 정책 (OnePlayer 0.4.0 계승):
//! - scene prepare window: 12초 (T-12초에 준비 시작)
//! - 영상 preroll window: 8초 (T-8초에 preroll 시작)
//! - 표출 시점 다운로드 금지 — 에셋 미준비 scene은 전환하지 않고 재시도
//! - 실제 정밀 전환(T-1초 이후 frame loop)은 렌더 스레드가 담당
//!
//! 루프 단계 번호 (`step` 필드):
//! 1. 세대 확인 — 타임라인 교체 시 구세대 루프 종료
//! 2. 다음 scene 선정 — 없으면 5초 후 재시도
//! 3. T-12초(prepare window) 전이면 그 시각까지 대기
//! 4. 에셋 준비 확인 — 미준비면 5초 후 재시도
//! 5. 영상 scene이면 T-8초(preroll window)까지 대기
//! 6. 렌더 스레드에 전환 명령 발송
//! 7. 다음 scene의 prepare window까지 대기

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};

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
        info!(step = 0, generation, scene_count = timeline.scenes.len(), "playback loop spawned");
        let engine = self.clone();
        tokio::spawn(async move {
            engine.playback_loop(timeline, generation).await;
        });
    }

    /// 재생 루프 본체. 다음 scene을 골라 prepare/전환을 예약하는 것을 반복한다.
    async fn playback_loop(self: Arc<Self>, timeline: PlaybackTimeline, generation: u64) {
        let mut dispatched = HashSet::new();
        loop {
            // step 1. 구세대 루프면 종료.
            let current_gen = self.playback_generation.load(Ordering::SeqCst);
            if current_gen != generation {
                info!(
                    step = 1,
                    generation,
                    current_gen,
                    "playback loop stopped (stale generation)"
                );
                break;
            }

            // step 2. 다음 표출 scene 선정.
            let now = self.clock.now_millis();
            let Some(next) = timeline.next_scene(now) else {
                debug!(step = 2, generation, now_millis = now, "no upcoming scene, retrying");
                self.set_state(EngineState::Ready).await;
                let _ = self
                    .events
                    .send(EngineEvent::Status("no upcoming scene".into()));
                sleep_ms(NO_SCENE_RETRY_MS).await;
                continue;
            };

            debug!(
                step = 2,
                generation,
                scene_id = %next.scene_id,
                start_time_millis = next.start_time_millis,
                has_video = next.has_video(),
                "next scene selected"
            );

            // 이미 전환 명령을 보낸 scene이면, 그 다음 scene의 prepare window까지 대기한다.
            if dispatched.contains(&next.scene_id) {
                let wake_at = next_wake_at(&timeline, next, now);
                let delay_ms = (wake_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS);
                debug!(
                    step = 2,
                    generation,
                    scene_id = %next.scene_id,
                    wake_at_millis = wake_at,
                    sleep_ms = delay_ms,
                    "scene already dispatched, waiting for next prepare window"
                );
                sleep_ms(delay_ms).await;
                continue;
            }

            // step 3. 아직 prepare window(T-12초) 전이면 진입 시각까지 대기.
            let prepare_at = next.start_time_millis - SCENE_PREPARE_WINDOW_MS;
            if now < prepare_at {
                let delay_ms = (prepare_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS);
                debug!(
                    step = 3,
                    generation,
                    scene_id = %next.scene_id,
                    prepare_at_millis = prepare_at,
                    sleep_ms = delay_ms,
                    "waiting for prepare window"
                );
                sleep_ms(delay_ms).await;
                continue;
            }

            // step 4. 에셋이 로컬에 없으면 전환하지 않는다 (표출 시점 다운로드 금지).
            if !self.scene_assets_ready(next) {
                warn!(
                    step = 4,
                    generation,
                    scene_id = %next.scene_id,
                    asset_count = next.asset_refs.len(),
                    "scene assets missing, retrying"
                );
                sleep_ms(NO_SCENE_RETRY_MS).await;
                continue;
            }

            // prepare 시작을 알린다 (실제 디코드/텍스처 업로드는 렌더 스레드).
            info!(
                step = 4,
                generation,
                scene_id = %next.scene_id,
                target_time_millis = next.start_time_millis,
                "scene prepare started"
            );
            self.set_state(EngineState::Preparing).await;
            let local_files = self.local_files.lock().await.clone();
            let _ = self.events.send(EngineEvent::ScenePrepared {
                scene_id: next.scene_id.clone(),
                target_time_millis: next.start_time_millis,
            });

            // step 5. 영상 scene은 preroll window(T-8초)에 맞춰 전환 명령을 보낸다.
            if next.has_video() {
                let preroll_at = next.start_time_millis - VIDEO_PREROLL_WINDOW_MS;
                let now = self.clock.now_millis();
                if now < preroll_at {
                    let delay_ms = (preroll_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS);
                    debug!(
                        step = 5,
                        generation,
                        scene_id = %next.scene_id,
                        preroll_at_millis = preroll_at,
                        sleep_ms = delay_ms,
                        "waiting for video preroll window"
                    );
                    sleep_ms(delay_ms).await;
                }
            }

            // step 6. 렌더 스레드로 전환 명령 발송. 수신자가 없으면 앱 종료로 보고 루프 탈출.
            self.set_state(EngineState::Ready).await;
            let cmd = SwitchCommand {
                scene: next.clone(),
                target_time_millis: next.start_time_millis,
                local_files,
            };
            if self.switch_tx.send(cmd).is_err() {
                warn!(
                    step = 6,
                    generation,
                    scene_id = %next.scene_id,
                    "switch command channel closed, stopping playback loop"
                );
                break;
            }
            info!(
                step = 6,
                generation,
                scene_id = %next.scene_id,
                target_time_millis = next.start_time_millis,
                "switch command dispatched"
            );
            dispatched.insert(next.scene_id.clone());

            // step 7. 현재 scene 표출이 끝날 때까지 기다리지 않고,
            //    바로 다음 scene의 prepare window까지 대기한다.
            let wake_at = next_wake_at(&timeline, next, self.clock.now_millis());
            let now = self.clock.now_millis();
            if wake_at > now {
                let delay_ms = (wake_at - now).max(MIN_PLAYBACK_LOOP_DELAY_MS);
                debug!(
                    step = 7,
                    generation,
                    scene_id = %next.scene_id,
                    wake_at_millis = wake_at,
                    sleep_ms = delay_ms,
                    "waiting for next prepare window"
                );
                sleep_ms(delay_ms).await;
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
            step = 8,
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
        warn!(step = 8, scene_id, reason, "switch failed");
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

/// 전환 명령을 보낸 뒤 다음으로 깨어날 시각을 계산한다.
///
/// 다음 scene이 있으면 그 scene의 prepare window(T-12초),
/// 없으면 현재 scene 종료 직전까지 대기한다.
fn next_wake_at(timeline: &PlaybackTimeline, scene: &PlaybackScene, now: i64) -> i64 {
    if let Some(prepare_at) = timeline.following_prepare_at(scene) {
        return prepare_at;
    }
    let end_at = scene.end_time_millis - MIN_PLAYBACK_LOOP_DELAY_MS;
    end_at.max(now + MIN_PLAYBACK_LOOP_DELAY_MS)
}
