//! `--sample` 데모 스케줄.
//!
//! Android 개발 Phase 1의 최소 동작 예제와 동일한 마일스톤:
//! - 실행 T+10초에 scene A(파란 배경) 표출
//! - T+20초에 scene B(주황 배경) 표출
//! - scene B는 표출 전에 미리 prepare되어 있어야 하고,
//!   전환 시점에는 레이어 alpha만 바뀌어야 한다
//! - 전환 delay(ms)가 로그로 기록된다

use std::collections::HashMap;
use std::sync::Mutex;

use oneplayer_core::clock::{Clock, SignageClock};
use oneplayer_core::timeline::{LayoutDefinition, LayoutElement, PlaybackScene};
use oneplayer_render::{DoubleBufferCompositor, ScenePreparer};
use tracing::info;

/// scene B의 예약 상태 (렌더 루프가 매 프레임 확인).
pub struct SampleSchedule {
    /// 표출할 scene B.
    pub scene_b: PlaybackScene,
    /// scene B prepare 시작 시각 (표출 12초 전 = T+8초).
    pub prepare_b_at: i64,
    /// scene B 전환 목표 시각 (T+20초).
    pub switch_b_at: i64,
    /// prepare 완료 여부 (중복 prepare 방지).
    pub b_prepared: bool,
}

/// scene B 스케줄을 만든다 (T+20초 표출, T+8초 prepare).
pub fn create_sample_schedule(clock: &SignageClock) -> SampleSchedule {
    let now = clock.now_millis();
    let scene_b = sample_scene("sample-b", now + 20_000, now + 40_000, "#aa4422");
    SampleSchedule {
        prepare_b_at: now + 8_000,
        switch_b_at: scene_b.start_time_millis,
        scene_b,
        b_prepared: false,
    }
}

/// scene A를 즉시 prepare하고 T+10초 전환을 예약한다 (앱 시작 시 1회).
pub fn bootstrap_sample_a(
    clock: &SignageClock,
    preparer: &Mutex<ScenePreparer>,
    compositor: &mut DoubleBufferCompositor,
) {
    let now = clock.now_millis();
    let scene_a = sample_scene("sample-a", now + 10_000, now + 20_000, "#2244aa");
    let local_files = HashMap::new();
    let Ok(mut preparer) = preparer.lock() else {
        return;
    };
    if let Ok(prepared_a) = preparer.prepare(&scene_a, &local_files, now) {
        compositor.preload(prepared_a);
        compositor.switch_at(scene_a.start_time_millis);
        info!(
            target_ms = scene_a.start_time_millis,
            "sample scene A scheduled"
        );
    }
}

/// prepare 시각(T+8초)이 되면 scene B를 준비하고 전환을 예약한다.
/// 렌더 루프가 매 프레임 호출하며, 한 번 완료되면 더 하지 않는다.
pub fn maybe_prepare_sample_b(
    now: i64,
    schedule: &mut SampleSchedule,
    preparer: &Mutex<ScenePreparer>,
    compositor: &mut DoubleBufferCompositor,
) {
    if schedule.b_prepared || now < schedule.prepare_b_at {
        return;
    }
    let local_files = HashMap::new();
    let Ok(mut preparer) = preparer.lock() else {
        return;
    };
    if let Ok(prepared_b) = preparer.prepare(&schedule.scene_b, &local_files, now) {
        compositor.preload(prepared_b);
        compositor.switch_at(schedule.switch_b_at);
        schedule.b_prepared = true;
        info!(target_ms = schedule.switch_b_at, "sample scene B scheduled");
    }
}

/// 단색 배경 텍스트 요소 하나로 구성된 데모 scene을 만든다.
fn sample_scene(id: &str, start: i64, end: i64, color: &str) -> PlaybackScene {
    PlaybackScene {
        scene_id: id.into(),
        schedule_id: 1,
        playlist_id: 1,
        item_id: 1,
        start_time_millis: start,
        end_time_millis: end,
        transition: None,
        loop_playback: false,
        layout: Some(LayoutDefinition {
            id: 1,
            name: id.into(),
            group_name: None,
            width: 1080,
            height: 1920,
            default_duration: Some(10),
            elements: vec![LayoutElement {
                id: "bg".into(),
                x: 0,
                y: 0,
                width: 1080,
                height: 1920,
                element_type: "text".into(),
                keep_aspect_ratio: false,
                file_id: None,
                content: Some(format!("OnePlayer sample {id}")),
                font: None,
                font_size: Some(48),
                bold: true,
                italic: false,
                underline: false,
                strikethrough: false,
                background_color: Some(color.into()),
                text_color: Some("#ffffff".into()),
                border_color: None,
                border_width: None,
                z_index: Some(1),
            }],
        }),
        asset_refs: vec![],
    }
}
