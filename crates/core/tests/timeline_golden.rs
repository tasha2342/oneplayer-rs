//! TimelineBuilder golden 테스트: 고정 입력 JSON → 기대 scene 목록 검증.
//! 기존 Android 앱과 동일한 확장 동작을 보장하기 위한 회귀 테스트.

use oneplayer_core::cms::PlaybackDataDto;
use oneplayer_core::timeline::TimelineBuilder;

/// 다중 item slot이 시간순으로 겹침 없이 확장되는지 확인한다.
#[test]
fn golden_timeline_expands_multi_item_slot() {
    let raw = include_str!("fixtures/play_data_golden.json");
    let data: PlaybackDataDto = serde_json::from_str(raw).expect("parse golden json");
    let now = chrono::DateTime::parse_from_rfc3339("2026-05-20T14:12:00+09:00")
        .expect("parse time")
        .timestamp_millis();
    let timeline = TimelineBuilder::build(&data, now);
    assert!(!timeline.scenes.is_empty());
    assert_eq!(timeline.device_id, "DV-1001");
    assert_eq!(timeline.revision, "golden-rev-1");
    assert!(timeline.scenes.len() >= 2);
    assert!(timeline
        .scenes
        .windows(2)
        .all(|w| w[0].end_time_millis <= w[1].start_time_millis));
}

/// 백엔드 공용 v1.1.0 예제가 그대로 파싱되고 RTB video/image가 일반 편성을
/// 겹치지 않게 대체하는지 확인한다.
#[test]
fn v1_1_contract_builds_rtb_overlays_with_fallbacks() {
    let raw = include_str!("../../../docs/api/play_data_v1.1.0.example.json");
    let data: PlaybackDataDto = serde_json::from_str(raw).expect("parse v1.1 example");
    assert_eq!(data.api_version.as_deref(), Some("1.1.0"));
    assert_eq!(data.rtb_slots.len(), 2);

    let now = chrono::DateTime::parse_from_rfc3339("2026-07-21T14:10:35+09:00")
        .expect("parse time")
        .timestamp_millis();
    let timeline = TimelineBuilder::build(&data, now);
    let rtb: Vec<_> = timeline
        .scenes
        .iter()
        .filter(|scene| scene.rtb.is_some())
        .collect();

    assert_eq!(rtb.len(), 6);
    assert!(rtb.iter().all(|scene| scene.fallback_scene.is_some()));
    assert!(rtb.iter().any(|scene| scene.has_video()));
    assert!(rtb.iter().any(|scene| {
        scene.layout.as_ref().is_some_and(|layout| {
            layout
                .elements
                .iter()
                .any(|element| element.element_type == "image")
        })
    }));
    assert!(timeline
        .scenes
        .windows(2)
        .all(|window| window[0].end_time_millis <= window[1].start_time_millis));
}

/// RTB가 없는 기존 응답은 추가 필드 없이도 계속 파싱되어야 한다.
#[test]
fn v1_0_contract_remains_backward_compatible() {
    let raw = include_str!("fixtures/play_data_golden.json");
    let data: PlaybackDataDto = serde_json::from_str(raw).expect("parse v1.0 fixture");
    assert!(data.api_version.is_none());
    assert!(data.rtb_slots.is_empty());
}

/// RTB 슬롯 하나가 깨져도 일반 편성과 나머지 유효 RTB는 유지되어야 한다.
#[test]
fn malformed_rtb_slot_is_ignored_without_losing_schedule() {
    let raw = include_str!("../../../docs/api/play_data_v1.1.0.example.json");
    let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
    value["rtb_slots"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "broken",
            "start_time": "14:30:00"
        }));

    let data: PlaybackDataDto = serde_json::from_value(value).expect("parse resilient payload");
    assert_eq!(data.slots.len(), 1);
    assert_eq!(data.rtb_slots.len(), 2);
}

#[test]
fn overlapping_rtb_slot_is_ignored_after_first_valid_slot() {
    let raw = include_str!("../../../docs/api/play_data_v1.1.0.example.json");
    let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
    let mut overlap = value["rtb_slots"][0].clone();
    overlap["id"] = serde_json::json!("overlap");
    overlap["start_time"] = serde_json::json!("14:10:45");
    overlap["end_time"] = serde_json::json!("14:11:15");
    value["rtb_slots"].as_array_mut().unwrap().push(overlap);
    let data: PlaybackDataDto = serde_json::from_value(value).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-21T14:10:35+09:00")
        .unwrap()
        .timestamp_millis();
    let timeline = TimelineBuilder::build(&data, now);

    assert!(timeline
        .scenes
        .iter()
        .filter_map(|scene| scene.rtb.as_ref())
        .all(|rtb| rtb.slot_id != "overlap"));
}

#[test]
fn rtb_slot_can_cross_midnight() {
    let raw = include_str!("../../../docs/api/play_data_v1.1.0.example.json");
    let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
    value["slots"][0]["start_time"] = serde_json::json!("23:00:00");
    value["slots"][0]["end_time"] = serde_json::json!("00:59:59");
    let first_rtb = value["rtb_slots"][0].clone();
    value["rtb_slots"] = serde_json::json!([first_rtb]);
    value["rtb_slots"][0]["id"] = serde_json::json!("midnight");
    value["rtb_slots"][0]["start_time"] = serde_json::json!("23:59:50");
    value["rtb_slots"][0]["end_time"] = serde_json::json!("00:00:20");
    let data: PlaybackDataDto = serde_json::from_value(value).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-07-21T23:59:55+09:00")
        .unwrap()
        .timestamp_millis();
    let timeline = TimelineBuilder::build(&data, now);
    let rtb: Vec<_> = timeline
        .scenes
        .iter()
        .filter(|scene| scene.rtb.is_some())
        .collect();

    assert_eq!(rtb.len(), 2);
    assert_eq!(
        rtb.iter().map(|scene| scene.duration_ms()).sum::<i64>(),
        30_000
    );
    assert!(rtb.iter().all(|scene| scene.fallback_scene.is_some()));
}
