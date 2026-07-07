//! CMS `play_data` 응답 → [`PlaybackTimeline`] 변환 빌더.
//!
//! 처리 순서:
//! 1. slot의 시작/종료 시각(HH:MM:SS)을 타임존 기준 epoch millis로 변환
//! 2. item이 1개면 slot 전체를 하나의 scene으로 사용
//! 3. item이 여러 개면 duration 기반 cycle을 현재 시각 window 안에서 확장
//! 4. 전체 scene을 시작 시각 기준으로 정렬해 타임라인 완성

use std::collections::HashMap;

use chrono::{Local, NaiveDate, NaiveTime};
use chrono_tz::Tz;

use crate::cms::{
    FileDownloadDto, LayoutDto, PlaybackAssetDto, PlaybackDataDto, PlaybackItemDto,
    PlaybackSlotDto,
};
use crate::config::{
    DAY_MS, DEFAULT_ITEM_DURATION_SECONDS, DEFAULT_ZONE_ID, MAX_EXPANDED_SCENES, ONE_SECOND_MS,
    TIMELINE_FUTURE_WINDOW_MS, TIMELINE_PAST_WINDOW_MS,
};

use super::models::{AssetRef, LayoutDefinition, LayoutElement, PlaybackScene, PlaybackTimeline};

/// file_id → 최상위 asset 메타데이터 조회 테이블.
type AssetLookup<'a> = HashMap<i64, &'a PlaybackAssetDto>;

/// CMS play_data를 재생 타임라인으로 변환한다.
pub struct TimelineBuilder;

impl TimelineBuilder {
    /// play_data 전체를 타임라인으로 변환한다.
    ///
    /// `now_millis`는 SignageClock 기준 현재 시각으로, 다중 item slot의
    /// 확장 window(과거 2분 / 미래 30분) 기준점이 된다.
    pub fn build(data: &PlaybackDataDto, now_millis: i64) -> PlaybackTimeline {
        let zone_id = Self::resolve_timezone(data);
        let local_date = Self::resolve_date(data);
        // 최상위 assets 목록을 file_id로 색인해 두고,
        // item의 file_downloads에 빠진 메타데이터(revision, size 등)를 보완할 때 사용.
        let asset_lookup: AssetLookup = data.assets.iter().map(|a| (a.file_id, a)).collect();

        let mut scenes = Vec::new();
        for slot in &data.slots {
            scenes.extend(Self::build_slot_scenes(
                data,
                &asset_lookup,
                slot,
                local_date,
                zone_id,
                now_millis,
            ));
        }

        // 시작 시각 → 종료 시각 → schedule_id 순으로 정렬.
        // 이후 PlaybackTimeline의 이진 탐색이 이 정렬을 전제로 한다.
        scenes.sort_by(|a, b| {
            a.start_time_millis
                .cmp(&b.start_time_millis)
                .then(a.end_time_millis.cmp(&b.end_time_millis))
                .then(a.schedule_id.cmp(&b.schedule_id))
        });

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
        slot: &PlaybackSlotDto,
        local_date: NaiveDate,
        zone_id: Tz,
        now_millis: i64,
    ) -> Vec<PlaybackScene> {
        let (slot_start_millis, slot_end_millis) =
            Self::slot_time_range(slot, local_date, zone_id);

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

    /// slot의 시작/종료 시각(HH:MM:SS)을 epoch millis 구간으로 변환한다.
    ///
    /// 종료 시각에는 +1초를 더해 "23:59:59까지"가 실제로 자정까지 포함되게 한다.
    /// 종료가 시작보다 빠르면 자정을 넘는 slot으로 보고 +1일 처리한다.
    fn slot_time_range(slot: &PlaybackSlotDto, local_date: NaiveDate, zone_id: Tz) -> (i64, i64) {
        let to_millis = |time_str: &str, extra_ms: i64| -> Option<i64> {
            let time = NaiveTime::parse_from_str(time_str, "%H:%M:%S").ok()?;
            local_date
                .and_time(time)
                .and_local_timezone(zone_id)
                .single()
                .map(|dt| dt.timestamp_millis() + extra_ms)
        };

        let start_millis = to_millis(&slot.start_time, 0).unwrap_or(0);
        let end_millis = to_millis(&slot.end_time, ONE_SECOND_MS).unwrap_or(start_millis);
        // 자정을 넘어가는 slot (예: 22:00 ~ 02:00) 처리.
        let slot_end_millis = if end_millis <= start_millis {
            end_millis + DAY_MS
        } else {
            end_millis
        };
        (start_millis, slot_end_millis)
    }
}

/// [`expand_slot_scenes`]의 인자 묶음.
/// 인자가 많아 실수를 줄이기 위해 구조체로 전달한다.
struct ExpandParams<'a> {
    data: &'a PlaybackDataDto,
    asset_lookup: &'a AssetLookup<'a>,
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

/// item 하나와 표출 구간으로 [`PlaybackScene`]을 만든다.
///
/// 에셋 참조는 item의 `file_downloads`를 우선 사용하고, 비어 있으면
/// layout 요소가 참조하는 file_id를 최상위 assets에서 찾아 보완한다.
fn build_scene(
    data: &PlaybackDataDto,
    asset_lookup: &AssetLookup,
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
        transition: item.transition.clone(),
        loop_playback: item.loop_enabled(),
        layout: item.layout.as_ref().map(layout_to_domain),
        asset_refs,
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
        .or_else(|| item.playback_data.as_ref().and_then(|p| p.duration).and_then(positive))
        .or_else(|| item.layout.as_ref().and_then(|l| l.default_duration).and_then(positive))
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
    let mut elements: Vec<LayoutElement> =
        dto.layout.iter().map(element_to_domain).collect();

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
