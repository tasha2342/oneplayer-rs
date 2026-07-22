//! 재생 타임라인 도메인.
//!
//! - [`models`]: 타임라인/장면/에셋 참조 등 순수 데이터 모델
//! - [`builder`]: CMS `play_data` 응답을 [`PlaybackTimeline`]으로 변환하는 빌더
//!
//! Android OnePlayer의 `PlaybackTimelineBuilder.kt`와 동일한 정책을 따른다:
//! slot item이 1개면 slot 구간 전체를 하나의 scene으로, 여러 개면 item duration
//! 기준으로 cycle 확장한다. 확장 범위는 과거 2분 / 미래 30분 window로 제한한다.

mod builder;
mod models;

pub use builder::TimelineBuilder;
pub use models::{
    AssetRef, LayoutDefinition, LayoutElement, PlaybackScene, PlaybackTimeline, RtbSceneMetadata,
    TrackingEvent, TrackingUrl,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cms::{PlaybackDataDto, PlaybackItemDto, PlaybackSlotDto};

    /// 단일 item slot을 가진 최소 play_data 샘플을 만든다.
    fn sample_data() -> PlaybackDataDto {
        PlaybackDataDto {
            api_version: None,
            device_id: "DV-1001".into(),
            date: "2026-05-20".into(),
            revision: "rev1".into(),
            contact_ip: None,
            server_time: None,
            generated_at: None,
            timezone: Some("Asia/Seoul".into()),
            slots: vec![PlaybackSlotDto {
                start_time: "14:10:00".into(),
                end_time: "14:15:00".into(),
                schedule_id: 101,
                playlist_id: 20,
                playlist_name: None,
                items: vec![PlaybackItemDto {
                    id: 1,
                    playlist_id: 20,
                    position: 0,
                    item_type: "image".into(),
                    ref_id: 1,
                    duration_seconds: 10,
                    transition: None,
                    loop_playback: false,
                    layout: None,
                    playback_data: None,
                    playback_data_b64: None,
                    file_downloads: vec![],
                }],
            }],
            assets: vec![],
            rtb_slots: vec![],
            override_data: None,
        }
    }

    /// item이 1개인 slot은 확장 없이 slot 전체 구간의 scene 하나가 되어야 한다.
    #[test]
    fn single_item_slot_produces_one_scene() {
        let data = sample_data();
        let timeline = TimelineBuilder::build(&data, 1_700_000_000_000);
        assert_eq!(timeline.scenes.len(), 1);
        assert_eq!(timeline.scenes[0].item_id, 1);
    }
}
