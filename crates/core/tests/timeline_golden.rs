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
    assert!(timeline.scenes.windows(2).all(|w| w[0].end_time_millis <= w[1].start_time_millis));
}
