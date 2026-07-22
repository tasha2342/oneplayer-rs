//! CMS `play_data` 응답 → [`PlaybackTimeline`] 변환 빌더.
//!
//! 처리 순서:
//! 1. slot의 시작/종료 시각(HH:MM:SS)을 타임존 기준 epoch millis로 변환
//! 2. item이 1개면 slot 전체를 하나의 scene으로 사용
//! 3. item이 여러 개면 duration 기반 cycle을 현재 시각 window 안에서 확장
//! 4. 전체 scene을 시작 시각 기준으로 정렬해 타임라인 완성

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::cms::{
    FileDownloadDto, LayoutDto, PlaybackAssetDto, PlaybackDataDto, PlaybackItemDto,
    PlaybackSlotDto, RtbItemDto, RtbSlotDto,
};
use crate::config::{
    DAY_MS, DEFAULT_ITEM_DURATION_SECONDS, DEFAULT_ZONE_ID, MAX_EXPANDED_SCENES, ONE_SECOND_MS,
    TIMELINE_FUTURE_WINDOW_MS, TIMELINE_PAST_WINDOW_MS,
};

use super::models::{
    AssetRef, LayoutDefinition, LayoutElement, PlaybackScene, PlaybackTimeline, RtbSceneMetadata,
    TrackingEvent, TrackingUrl,
};

/// file_id → 최상위 asset 메타데이터 조회 테이블.
type AssetLookup<'a> = HashMap<i64, &'a PlaybackAssetDto>;

/// layout id → 변환 완료된 도메인 레이아웃 공유 테이블.
///
/// play_data에는 동일 레이아웃이 slot마다 통째로 반복되므로,
/// id 기준으로 한 번만 변환하고 모든 scene이 `Arc`로 공유한다
/// (scene 수천 개 x 레이아웃 복제로 인한 메모리 낭비 방지).
type LayoutCache = HashMap<i64, Arc<LayoutDefinition>>;

/// CMS play_data를 재생 타임라인으로 변환한다.
pub struct TimelineBuilder;

impl TimelineBuilder {
    /// play_data 전체를 타임라인으로 변환한다.
    ///
    /// `now_millis`는 SignageClock 기준 현재 시각으로, 다중 item slot의
    /// 확장 window(과거 2분 / 미래 30분) 기준점이 된다.
    pub fn build(data: &PlaybackDataDto, now_millis: i64) -> PlaybackTimeline {
        info!(
            api_stage = "timeline_build_started",
            revision = %data.revision,
            normal_slot_count = data.slots.len(),
            rtb_slot_count = data.rtb_slots.len(),
            now_millis,
            "playback timeline build started"
        );
        let zone_id = Self::resolve_timezone(data);
        let local_date = Self::resolve_date(data);
        // 최상위 assets 목록을 file_id로 색인해 두고,
        // item의 file_downloads에 빠진 메타데이터(revision, size 등)를 보완할 때 사용.
        let asset_lookup: AssetLookup = data.assets.iter().map(|a| (a.file_id, a)).collect();

        // 레이아웃을 id 기준으로 한 번만 변환해 둔다 (scene들이 Arc 공유).
        let mut layout_cache: LayoutCache = HashMap::new();
        for slot in &data.slots {
            for item in &slot.items {
                if let Some(layout) = &item.layout {
                    layout_cache
                        .entry(layout.id)
                        .or_insert_with(|| Arc::new(layout_to_domain(layout)));
                }
            }
        }

        let mut baseline_scenes = Vec::new();
        for slot in &data.slots {
            baseline_scenes.extend(Self::build_slot_scenes(
                data,
                &asset_lookup,
                &layout_cache,
                slot,
                local_date,
                zone_id,
                now_millis,
            ));
        }
        baseline_scenes.sort_by_key(|scene| scene.start_time_millis);

        let rtb_scenes =
            Self::build_rtb_scenes(data, &baseline_scenes, local_date, zone_id, now_millis);
        let mut scenes = overlay_rtb_scenes(&baseline_scenes, rtb_scenes);

        // 시작 시각 → 종료 시각 → schedule_id 순으로 정렬.
        // 이후 PlaybackTimeline의 이진 탐색이 이 정렬을 전제로 한다.
        scenes.sort_by(|a, b| {
            a.start_time_millis
                .cmp(&b.start_time_millis)
                .then(a.end_time_millis.cmp(&b.end_time_millis))
                .then(a.schedule_id.cmp(&b.schedule_id))
        });
        let rtb_scene_count = scenes.iter().filter(|scene| scene.rtb.is_some()).count();
        info!(
            api_stage = "timeline_build_completed",
            revision = %data.revision,
            baseline_scene_count = baseline_scenes.len(),
            rtb_scene_count,
            effective_scene_count = scenes.len(),
            "playback timeline build completed"
        );

        PlaybackTimeline {
            device_id: data.device_id.clone(),
            date: data.date.clone(),
            revision: data.revision.clone(),
            server_time: data.server_time.clone(),
            generated_at: data.generated_at.clone(),
            timezone: data
                .timezone
                .clone()
                .unwrap_or_else(|| DEFAULT_ZONE_ID.to_string()),
            scenes,
        }
    }

    /// RTB 슬롯을 full-screen image/video 장면으로 확장한다.
    fn build_rtb_scenes(
        data: &PlaybackDataDto,
        baseline: &[PlaybackScene],
        local_date: NaiveDate,
        zone_id: Tz,
        now_millis: i64,
    ) -> Vec<PlaybackScene> {
        let mut slots: Vec<_> = data
            .rtb_slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                let range =
                    time_range(&slot.start_time, &slot.end_time, local_date, zone_id, false);
                (index, slot, range)
            })
            .collect();
        slots.sort_by_key(|(index, _, (start, _))| (*start, *index));

        let mut result = Vec::new();
        let mut accepted_end = i64::MIN;
        for (_, slot, (slot_start, slot_end)) in slots {
            if slot_start < accepted_end {
                warn!(
                    api_stage = "rtb_slot_rejected",
                    reason = "overlapping_slot",
                    slot_id = %slot.id,
                    slot_start_millis = slot_start,
                    slot_end_millis = slot_end,
                    previous_accepted_end_millis = accepted_end,
                    "ignoring overlapping RTB slot"
                );
                continue;
            }
            match build_rtb_slot_scenes(data, slot, baseline, slot_start, slot_end, now_millis) {
                Some(mut scenes) if !scenes.is_empty() => {
                    info!(
                        api_stage = "rtb_slot_accepted",
                        slot_id = %slot.id,
                        request_id = slot.request_id.as_deref().unwrap_or(""),
                        slot_start_millis = slot_start,
                        slot_end_millis = slot_end,
                        item_count = slot.items.len(),
                        generated_scene_count = scenes.len(),
                        "RTB slot accepted"
                    );
                    accepted_end = slot_end;
                    result.append(&mut scenes);
                }
                Some(_) => debug!(
                    api_stage = "rtb_slot_outside_window",
                    slot_id = %slot.id,
                    slot_start_millis = slot_start,
                    slot_end_millis = slot_end,
                    "RTB slot is valid but outside current expansion window"
                ),
                None => warn!(
                    api_stage = "rtb_slot_rejected",
                    reason = "invalid_slot",
                    slot_id = %slot.id,
                    request_id = slot.request_id.as_deref().unwrap_or(""),
                    slot_start_millis = slot_start,
                    slot_end_millis = slot_end,
                    item_count = slot.items.len(),
                    "ignoring invalid RTB slot"
                ),
            }
        }
        result
    }

    /// play_data의 timezone 문자열을 파싱한다. 실패하면 Asia/Seoul을 사용한다.
    fn resolve_timezone(data: &PlaybackDataDto) -> Tz {
        data.timezone
            .as_deref()
            .unwrap_or(DEFAULT_ZONE_ID)
            .parse()
            .unwrap_or(chrono_tz::Asia::Seoul)
    }

    /// play_data의 날짜(`YYYY-MM-DD`)를 파싱한다. 실패하면 오늘 날짜를 사용한다.
    fn resolve_date(data: &PlaybackDataDto) -> NaiveDate {
        NaiveDate::parse_from_str(&data.date, "%Y-%m-%d")
            .unwrap_or_else(|_| Local::now().date_naive())
    }

    /// slot 하나를 scene 목록으로 변환한다.
    ///
    /// - item 0개: 빈 목록
    /// - item 1개: slot 구간 전체를 하나의 scene으로
    /// - item 여러 개: duration 기반 cycle을 window 안에서 확장
    fn build_slot_scenes(
        data: &PlaybackDataDto,
        asset_lookup: &AssetLookup,
        layout_cache: &LayoutCache,
        slot: &PlaybackSlotDto,
        local_date: NaiveDate,
        zone_id: Tz,
        now_millis: i64,
    ) -> Vec<PlaybackScene> {
        let (slot_start_millis, slot_end_millis) =
            time_range(&slot.start_time, &slot.end_time, local_date, zone_id, true);

        // position 순으로 item을 정렬해 재생 순서를 고정한다.
        let mut items = slot.items.clone();
        items.sort_by_key(|i| i.position);
        if items.is_empty() {
            return Vec::new();
        }

        // item이 하나뿐이면 slot 구간을 그대로 사용한다 (확장 불필요).
        if items.len() == 1 {
            return vec![build_scene(
                data,
                asset_lookup,
                layout_cache,
                slot.schedule_id,
                slot.playlist_id,
                &items[0],
                slot_start_millis,
                slot_end_millis,
            )];
        }

        // item이 여러 개면 cycle(전체 item duration 합)을 반복 확장한다.
        let durations: Vec<i64> = items.iter().map(item_duration_millis).collect();
        let cycle_millis: i64 = durations.iter().sum();
        if cycle_millis <= 0 {
            return Vec::new();
        }

        // 메모리 제한을 위해 현재 시각 기준 과거/미래 window로만 확장한다.
        let window_start = (now_millis - TIMELINE_PAST_WINDOW_MS).max(slot_start_millis);
        let window_end = (now_millis + TIMELINE_FUTURE_WINDOW_MS).min(slot_end_millis);
        if window_start >= window_end {
            return Vec::new();
        }

        expand_slot_scenes(ExpandParams {
            data,
            asset_lookup,
            layout_cache,
            schedule_id: slot.schedule_id,
            playlist_id: slot.playlist_id,
            items: &items,
            durations: &durations,
            slot_start_millis,
            slot_end_millis,
            window_start_millis: window_start,
            window_end_millis: window_end,
            cycle_millis,
        })
    }
}

/// slot의 시작/종료 시각(HH:MM:SS)을 epoch millis 구간으로 변환한다.
/// 기존 일반 슬롯만 마지막 초를 포함하고 RTB는 `[start, end)`를 사용한다.
fn time_range(
    start_time: &str,
    end_time: &str,
    local_date: NaiveDate,
    zone_id: Tz,
    inclusive_last_second: bool,
) -> (i64, i64) {
    let to_millis = |time_str: &str, extra_ms: i64| -> Option<i64> {
        let time = NaiveTime::parse_from_str(time_str, "%H:%M:%S").ok()?;
        local_date
            .and_time(time)
            .and_local_timezone(zone_id)
            .single()
            .map(|dt| dt.timestamp_millis() + extra_ms)
    };

    let start_millis = to_millis(start_time, 0).unwrap_or(0);
    let end_extra = if inclusive_last_second {
        ONE_SECOND_MS
    } else {
        0
    };
    let end_millis = to_millis(end_time, end_extra).unwrap_or(start_millis);
    let slot_end_millis = if end_millis <= start_millis {
        end_millis + DAY_MS
    } else {
        end_millis
    };
    (start_millis, slot_end_millis)
}

/// [`expand_slot_scenes`]의 인자 묶음.
/// 인자가 많아 실수를 줄이기 위해 구조체로 전달한다.
struct ExpandParams<'a> {
    data: &'a PlaybackDataDto,
    asset_lookup: &'a AssetLookup<'a>,
    layout_cache: &'a LayoutCache,
    schedule_id: i64,
    playlist_id: i64,
    items: &'a [PlaybackItemDto],
    durations: &'a [i64],
    slot_start_millis: i64,
    slot_end_millis: i64,
    window_start_millis: i64,
    window_end_millis: i64,
    cycle_millis: i64,
}

/// 다중 item slot을 window 구간 안에서 개별 scene들로 확장한다.
///
/// slot 시작부터 item들이 순서대로 반복(cycle)된다고 보고,
/// window 시작 시점에 어느 item의 어느 지점이 재생 중인지 역산한 뒤
/// window 끝까지 scene을 순차 생성한다.
fn expand_slot_scenes(p: ExpandParams) -> Vec<PlaybackScene> {
    // window 시작 시점이 cycle의 어느 위치인지 계산.
    let cycle_offset = (p.window_start_millis - p.slot_start_millis).max(0) % p.cycle_millis;

    // cycle_offset이 속한 item과, 그 item 시작까지의 누적 offset을 찾는다.
    let mut item_index = 0usize;
    let mut offset_before_item = 0i64;
    while item_index < p.durations.len().saturating_sub(1)
        && cycle_offset >= offset_before_item + p.durations[item_index]
    {
        offset_before_item += p.durations[item_index];
        item_index += 1;
    }

    // cursor를 해당 item의 시작 시각으로 되돌린 뒤 window 끝까지 전진하며 scene 생성.
    let mut cursor = p.window_start_millis - (cycle_offset - offset_before_item);
    let mut scenes = Vec::new();
    while cursor < p.window_end_millis && scenes.len() < MAX_EXPANDED_SCENES {
        let item = &p.items[item_index];
        let scene_end = (cursor + p.durations[item_index]).min(p.slot_end_millis);
        // window 시작 이전에 이미 끝난 scene은 제외한다.
        if scene_end > p.window_start_millis {
            scenes.push(build_scene(
                p.data,
                p.asset_lookup,
                p.layout_cache,
                p.schedule_id,
                p.playlist_id,
                item,
                cursor,
                scene_end,
            ));
        }
        cursor = scene_end;
        item_index = (item_index + 1) % p.items.len();
    }
    scenes
}

fn build_rtb_slot_scenes(
    data: &PlaybackDataDto,
    slot: &RtbSlotDto,
    baseline: &[PlaybackScene],
    slot_start: i64,
    slot_end: i64,
    now_millis: i64,
) -> Option<Vec<PlaybackScene>> {
    if slot.id.trim().is_empty() {
        warn!(
            api_stage = "rtb_slot_validation_failed",
            reason = "empty_slot_id",
            request_id = slot.request_id.as_deref().unwrap_or(""),
            "RTB slot validation failed"
        );
        return None;
    }
    if slot.items.is_empty() {
        warn!(
            api_stage = "rtb_slot_validation_failed",
            reason = "empty_items",
            slot_id = %slot.id,
            request_id = slot.request_id.as_deref().unwrap_or(""),
            "RTB slot validation failed"
        );
        return None;
    }
    if slot_start >= slot_end {
        warn!(
            api_stage = "rtb_slot_validation_failed",
            reason = "invalid_time_range",
            slot_id = %slot.id,
            slot_start_millis = slot_start,
            slot_end_millis = slot_end,
            "RTB slot validation failed"
        );
        return None;
    }

    let mut items = slot.items.clone();
    items.sort_by_key(|item| item.position);
    for item in &items {
        if let Err(reason) = validate_rtb_item(item) {
            warn!(
                api_stage = "rtb_item_validation_failed",
                reason,
                slot_id = %slot.id,
                request_id = slot.request_id.as_deref().unwrap_or(""),
                bid_id = %item.bid_id,
                creative_id = %item.creative_id,
                position = item.position,
                asset_type = %item.asset.asset_type,
                mime_type = %item.asset.mime_type,
                width = item.asset.width,
                height = item.asset.height,
                duration_seconds = item.asset.duration_seconds,
                "RTB item validation failed; entire RTB slot will fallback"
            );
            return None;
        }
    }

    let durations: Vec<i64> = items
        .iter()
        .map(|item| item.asset.duration_seconds * ONE_SECOND_MS)
        .collect();
    let cycle_ms: i64 = durations.iter().sum();
    if cycle_ms <= 0 {
        warn!(
            api_stage = "rtb_slot_validation_failed",
            reason = "non_positive_cycle_duration",
            slot_id = %slot.id,
            cycle_millis = cycle_ms,
            "RTB slot validation failed"
        );
        return None;
    }

    let window_start = (now_millis - TIMELINE_PAST_WINDOW_MS).max(slot_start);
    let window_end = (now_millis + TIMELINE_FUTURE_WINDOW_MS).min(slot_end);
    if window_start >= window_end {
        return Some(Vec::new());
    }

    let cycle_offset = (window_start - slot_start).max(0) % cycle_ms;
    let mut item_index = 0usize;
    let mut offset_before_item = 0i64;
    while item_index < durations.len().saturating_sub(1)
        && cycle_offset >= offset_before_item + durations[item_index]
    {
        offset_before_item += durations[item_index];
        item_index += 1;
    }

    let mut cursor = window_start - (cycle_offset - offset_before_item);
    let mut scenes = Vec::new();
    while cursor < window_end && scenes.len() < MAX_EXPANDED_SCENES {
        let item = &items[item_index];
        let scene_end = (cursor + durations[item_index]).min(slot_end);
        if scene_end > window_start {
            scenes.push(build_rtb_scene(
                data, slot, item, cursor, scene_end, baseline,
            ));
        }
        cursor = scene_end;
        item_index = (item_index + 1) % items.len();
    }
    Some(scenes)
}

fn validate_rtb_item(item: &RtbItemDto) -> Result<(), &'static str> {
    let asset = &item.asset;
    if !matches!(
        (asset.asset_type.as_str(), asset.mime_type.as_str()),
        ("video", "video/mp4") | ("image", "image/jpeg") | ("image", "image/png")
    ) {
        return Err("unsupported_asset_type_or_mime");
    }
    if item.bid_id.trim().is_empty() {
        return Err("empty_bid_id");
    }
    if item.creative_id.trim().is_empty() {
        return Err("empty_creative_id");
    }
    if !asset.download_url.starts_with("https://") {
        return Err("asset_url_must_be_https");
    }
    if asset.width <= 0 || asset.height <= 0 {
        return Err("non_positive_asset_dimensions");
    }
    if asset.duration_seconds <= 0 {
        return Err("non_positive_asset_duration");
    }
    if asset.size_bytes.is_some_and(|size| size <= 0) {
        return Err("non_positive_asset_size");
    }
    Ok(())
}

fn build_rtb_scene(
    data: &PlaybackDataDto,
    slot: &RtbSlotDto,
    item: &RtbItemDto,
    start_millis: i64,
    end_millis: i64,
    baseline: &[PlaybackScene],
) -> PlaybackScene {
    let file_id = stable_id(&format!("rtb-file:{}", item.creative_id));
    let layout_id = stable_id(&format!("rtb-layout:{}", item.creative_id));
    let scene_id = format!(
        "{}:rtb:{}:{}:{}",
        data.revision, slot.id, item.bid_id, start_millis
    );
    let layout = Arc::new(LayoutDefinition {
        id: layout_id,
        name: format!("RTB {}", item.creative_id),
        group_name: Some("rtb".to_string()),
        width: item.asset.width,
        height: item.asset.height,
        elements: vec![LayoutElement {
            id: format!("rtb-asset-{}", item.creative_id),
            x: 0,
            y: 0,
            width: item.asset.width,
            height: item.asset.height,
            element_type: item.asset.asset_type.clone(),
            keep_aspect_ratio: true,
            file_id: Some(file_id),
            content: None,
            font: None,
            font_size: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            background_color: Some("#000000".to_string()),
            text_color: None,
            border_color: None,
            border_width: None,
            z_index: Some(0),
        }],
        default_duration: Some(item.asset.duration_seconds),
    });

    let fallback_scene = baseline
        .iter()
        .find(|scene| {
            scene.start_time_millis <= start_millis && scene.end_time_millis > start_millis
        })
        .or_else(|| {
            baseline.iter().find(|scene| {
                scene.end_time_millis > start_millis && scene.start_time_millis < end_millis
            })
        })
        .map(|scene| {
            let mut fallback = scene.clone();
            fallback.scene_id = format!("fallback:{scene_id}:{}", scene.scene_id);
            fallback.start_time_millis = start_millis;
            fallback.end_time_millis = end_millis;
            fallback.rtb = None;
            fallback.fallback_scene = None;
            Box::new(fallback)
        });
    if fallback_scene.is_none() {
        warn!(
            api_stage = "rtb_fallback_missing",
            slot_id = %slot.id,
            bid_id = %item.bid_id,
            creative_id = %item.creative_id,
            scene_id = %scene_id,
            start_time_millis = start_millis,
            end_time_millis = end_millis,
            "RTB scene has no overlapping baseline scene; failure may leave current screen unchanged"
        );
    }

    let mut tracking = Vec::new();
    for entry in &item.tracking {
        let Some(event) = TrackingEvent::parse(&entry.event) else {
            warn!(
                api_stage = "tracking_definition_rejected",
                reason = "unknown_event",
                slot_id = %slot.id,
                bid_id = %item.bid_id,
                creative_id = %item.creative_id,
                event = %entry.event,
                "unknown tracking event ignored"
            );
            continue;
        };
        if !(entry.url.starts_with("https://") || entry.url.starts_with("http://")) {
            warn!(
                api_stage = "tracking_definition_rejected",
                reason = "invalid_url_scheme",
                slot_id = %slot.id,
                bid_id = %item.bid_id,
                creative_id = %item.creative_id,
                event = event.as_str(),
                url = %url_without_query(&entry.url),
                "tracking URL ignored"
            );
            continue;
        }
        tracking.push(TrackingUrl {
            event,
            url: entry.url.clone(),
        });
    }
    debug!(
        api_stage = "rtb_scene_created",
        slot_id = %slot.id,
        request_id = slot.request_id.as_deref().unwrap_or(""),
        bid_id = %item.bid_id,
        creative_id = %item.creative_id,
        scene_id = %scene_id,
        asset_type = %item.asset.asset_type,
        mime_type = %item.asset.mime_type,
        start_time_millis = start_millis,
        end_time_millis = end_millis,
        tracking_count = tracking.len(),
        has_fallback = fallback_scene.is_some(),
        "RTB scene created"
    );

    PlaybackScene {
        scene_id,
        schedule_id: stable_id(&format!("rtb-schedule:{}", slot.id)),
        playlist_id: stable_id(&format!("rtb-playlist:{}", slot.id)),
        item_id: stable_id(&format!("rtb-item:{}", item.bid_id)),
        start_time_millis: start_millis,
        end_time_millis: end_millis,
        transition: None,
        loop_playback: false,
        layout: Some(layout),
        asset_refs: vec![AssetRef {
            file_id,
            revision: item.creative_id.clone(),
            download_url: item.asset.download_url.clone(),
            mime_type: Some(item.asset.mime_type.clone()),
            size_bytes: item.asset.size_bytes,
            checksum: item.asset.checksum.clone(),
        }],
        rtb: Some(RtbSceneMetadata {
            slot_id: slot.id.clone(),
            request_id: slot.request_id.clone(),
            bid_id: item.bid_id.clone(),
            imp_id: item.imp_id.clone(),
            ad_id: item.ad_id.clone(),
            creative_id: item.creative_id.clone(),
            price: item.price,
            currency: slot.currency.clone().unwrap_or_else(|| "KRW".to_string()),
            tracking,
        }),
        fallback_scene,
    }
}

/// 일반 scene에서 RTB 구간을 잘라낸 뒤 RTB scene을 삽입한다.
fn overlay_rtb_scenes(
    baseline: &[PlaybackScene],
    mut rtb_scenes: Vec<PlaybackScene>,
) -> Vec<PlaybackScene> {
    if rtb_scenes.is_empty() {
        return baseline.to_vec();
    }
    rtb_scenes.sort_by_key(|scene| scene.start_time_millis);
    let mut result = Vec::new();
    for base in baseline {
        let mut fragments = vec![base.clone()];
        for rtb in &rtb_scenes {
            let mut next = Vec::new();
            for fragment in fragments {
                if rtb.end_time_millis <= fragment.start_time_millis
                    || rtb.start_time_millis >= fragment.end_time_millis
                {
                    next.push(fragment);
                    continue;
                }
                if fragment.start_time_millis < rtb.start_time_millis {
                    let mut before = fragment.clone();
                    before.end_time_millis = rtb.start_time_millis;
                    before.scene_id =
                        format!("{}:segment:{}", before.scene_id, before.start_time_millis);
                    next.push(before);
                }
                if fragment.end_time_millis > rtb.end_time_millis {
                    let mut after = fragment;
                    after.start_time_millis = rtb.end_time_millis;
                    after.scene_id =
                        format!("{}:segment:{}", after.scene_id, after.start_time_millis);
                    next.push(after);
                }
            }
            fragments = next;
        }
        result.extend(fragments);
    }
    result.extend(rtb_scenes);
    result
}

fn stable_id(value: &str) -> i64 {
    let digest = Sha256::digest(value.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    -((i64::from_be_bytes(bytes) & i64::MAX).max(1))
}

fn url_without_query(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// item 하나와 표출 구간으로 [`PlaybackScene`]을 만든다.
///
/// 에셋 참조는 item의 `file_downloads`를 우선 사용하고, 비어 있으면
/// layout 요소가 참조하는 file_id를 최상위 assets에서 찾아 보완한다.
fn build_scene(
    data: &PlaybackDataDto,
    asset_lookup: &AssetLookup,
    layout_cache: &LayoutCache,
    schedule_id: i64,
    playlist_id: i64,
    item: &PlaybackItemDto,
    start_millis: i64,
    end_millis: i64,
) -> PlaybackScene {
    let asset_refs = collect_asset_refs(data, asset_lookup, item);

    PlaybackScene {
        scene_id: format!(
            "{}:{}:{}:{}",
            data.revision, schedule_id, item.id, start_millis
        ),
        schedule_id,
        playlist_id,
        item_id: item.id,
        start_time_millis: start_millis,
        end_time_millis: end_millis,
        transition: item.transition.clone().or_else(|| {
            item.playback_data
                .as_ref()
                .and_then(|data| data.transition.clone())
        }),
        loop_playback: item.loop_enabled(),
        // 미리 변환해 둔 공유 레이아웃을 참조한다 (복제 없음).
        layout: item
            .layout
            .as_ref()
            .and_then(|l| layout_cache.get(&l.id).cloned()),
        asset_refs,
        rtb: None,
        fallback_scene: None,
    }
}

/// item이 필요로 하는 에셋 참조 목록을 수집한다.
///
/// 1순위: item의 `file_downloads` (다운로드 URL이 명시됨)
/// 2순위: layout 요소들의 `file_id`를 최상위 assets에서 조회
fn collect_asset_refs(
    data: &PlaybackDataDto,
    asset_lookup: &AssetLookup,
    item: &PlaybackItemDto,
) -> Vec<AssetRef> {
    if !item.file_downloads.is_empty() {
        return item
            .file_downloads
            .iter()
            .map(|fd| file_download_to_ref(fd, &data.revision, asset_lookup.get(&fd.file_id)))
            .collect();
    }

    item.layout
        .as_ref()
        .map(|layout| {
            layout
                .layout
                .iter()
                .filter_map(|el| el.file_id.and_then(|fid| asset_lookup.get(&fid)))
                .map(|asset| asset_to_ref(asset, &data.revision))
                .collect()
        })
        .unwrap_or_default()
}

/// item의 재생 시간(ms)을 결정한다.
///
/// 우선순위: item.duration_seconds → playback_data.duration
/// → layout.default_duration → 기본값 15초.
fn item_duration_millis(item: &PlaybackItemDto) -> i64 {
    let positive = |v: i64| if v > 0 { Some(v) } else { None };
    let seconds = positive(item.duration_seconds)
        .or_else(|| {
            item.playback_data
                .as_ref()
                .and_then(|p| p.duration)
                .and_then(positive)
        })
        .or_else(|| {
            item.layout
                .as_ref()
                .and_then(|l| l.default_duration)
                .and_then(positive)
        })
        .unwrap_or(DEFAULT_ITEM_DURATION_SECONDS);
    seconds * ONE_SECOND_MS
}

/// `file_downloads` 항목을 [`AssetRef`]로 변환한다.
/// 누락된 필드는 최상위 asset 메타데이터 → 타임라인 revision 순으로 보완한다.
fn file_download_to_ref(
    fd: &FileDownloadDto,
    timeline_revision: &str,
    asset: Option<&&PlaybackAssetDto>,
) -> AssetRef {
    AssetRef {
        file_id: fd.file_id,
        revision: fd
            .revision
            .clone()
            .or_else(|| asset.and_then(|a| a.revision.clone()))
            .unwrap_or_else(|| timeline_revision.to_string()),
        download_url: fd.download_url.clone(),
        mime_type: fd
            .mime_type
            .clone()
            .or_else(|| asset.and_then(|a| a.mime_type.clone())),
        size_bytes: fd.size_bytes.or_else(|| asset.and_then(|a| a.size_bytes)),
        checksum: fd
            .checksum
            .clone()
            .or_else(|| asset.and_then(|a| a.checksum.clone())),
    }
}

/// 최상위 asset 메타데이터를 [`AssetRef`]로 변환한다.
fn asset_to_ref(asset: &PlaybackAssetDto, timeline_revision: &str) -> AssetRef {
    AssetRef {
        file_id: asset.file_id,
        revision: asset
            .revision
            .clone()
            .unwrap_or_else(|| timeline_revision.to_string()),
        download_url: asset.download_url.clone(),
        mime_type: asset.mime_type.clone(),
        size_bytes: asset.size_bytes,
        checksum: asset.checksum.clone(),
    }
}

/// CMS layout DTO를 도메인 모델로 변환한다.
///
/// 정책: scene당 video 요소는 최대 1개만 유지한다 (DID decoder 안정성).
/// video가 여러 개면 z_index가 가장 높은 것만 남긴다.
/// 최종 요소 목록은 z_index 오름차순으로 정렬한다 (그리기 순서).
fn layout_to_domain(dto: &LayoutDto) -> LayoutDefinition {
    let mut elements: Vec<LayoutElement> = dto.layout.iter().map(element_to_domain).collect();

    retain_single_video(&mut elements);
    elements.sort_by_key(|e| e.z_index.unwrap_or(0));

    LayoutDefinition {
        id: dto.id,
        name: dto.name.clone(),
        group_name: dto.group_name.clone(),
        width: dto.width,
        height: dto.height,
        elements,
        default_duration: dto.default_duration,
    }
}

/// layout 요소 DTO 하나를 도메인 모델로 변환한다.
fn element_to_domain(el: &crate::cms::LayoutElementDto) -> LayoutElement {
    LayoutElement {
        id: el.id.clone(),
        x: el.x,
        y: el.y,
        width: el.width,
        height: el.height,
        element_type: el.element_type.clone(),
        keep_aspect_ratio: el.keep_aspect_ratio,
        file_id: el.file_id,
        content: el.content.clone(),
        font: el.font.clone(),
        font_size: el.font_size,
        bold: el.bold,
        italic: el.italic,
        underline: el.underline,
        strikethrough: el.strikethrough,
        background_color: el.background_color.clone(),
        text_color: el.text_color.clone(),
        border_color: el.border_color.clone(),
        border_width: el.border_width,
        z_index: el.z_index,
    }
}

/// video 요소가 2개 이상이면 z_index가 가장 높은 하나만 남긴다.
fn retain_single_video(elements: &mut Vec<LayoutElement>) {
    let video_count = elements
        .iter()
        .filter(|e| e.element_type == "video")
        .count();
    if video_count <= 1 {
        return;
    }
    let keep_id = elements
        .iter()
        .filter(|e| e.element_type == "video")
        .max_by_key(|e| e.z_index.unwrap_or(0))
        .map(|e| e.id.clone())
        .expect("video_count > 1 guarantees at least one video element");
    elements.retain(|e| e.element_type != "video" || e.id == keep_id);
}
