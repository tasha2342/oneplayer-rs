//! 타임라인 데이터 모델: 에셋 참조, 레이아웃, 장면(scene), 타임라인.
//!
//! 이 파일에는 로직이 거의 없는 순수 데이터 구조만 둔다.
//! 변환/빌드 로직은 [`super::builder`]에 있다.

use serde::{Deserialize, Serialize};

/// 다운로드해야 할 콘텐츠 파일 하나에 대한 참조.
///
/// 동일 `file_id`라도 `revision`이 다르면 다른 파일로 취급한다
/// (캐시 키가 `{file_id}_{revision}`이기 때문).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetRef {
    pub file_id: i64,
    pub revision: String,
    pub download_url: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
}

impl AssetRef {
    /// 로컬 캐시 파일명에 쓰이는 키(`{file_id}_{revision}`)를 만든다.
    pub fn cache_key(&self) -> String {
        format!("{}_{}", self.file_id, self.revision)
    }
}

/// 레이아웃 안의 요소 하나 (이미지 / 영상 / 텍스트 등).
///
/// 좌표(x, y, width, height)는 레이아웃 기준 해상도 좌표이며,
/// 실제 표출 시 렌더러가 화면 해상도에 맞게 스케일링한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutElement {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 요소 종류: `"image"`, `"video"`, `"text"` 등.
    pub element_type: String,
    pub keep_aspect_ratio: bool,
    /// 이미지/영상 요소가 참조하는 파일 ID.
    pub file_id: Option<i64>,
    /// 텍스트 요소의 내용.
    pub content: Option<String>,
    pub font: Option<String>,
    pub font_size: Option<i32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub border_color: Option<String>,
    pub border_width: Option<i32>,
    /// 그리기 순서. 값이 클수록 위에 그려진다.
    pub z_index: Option<i32>,
}

/// 하나의 화면 구성(레이아웃) 정의.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutDefinition {
    pub id: i64,
    pub name: String,
    pub group_name: Option<String>,
    /// 레이아웃 기준 해상도 (실제 화면과 다를 수 있음).
    pub width: i32,
    pub height: i32,
    /// z_index 오름차순으로 정렬된 요소 목록.
    pub elements: Vec<LayoutElement>,
    pub default_duration: Option<i64>,
}

/// 특정 시각 구간에 표출될 하나의 완성된 장면.
///
/// `start_time_millis` ~ `end_time_millis`(epoch millis, SignageClock 기준)
/// 동안 이 장면이 화면에 유지된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackScene {
    /// `{revision}:{schedule_id}:{item_id}:{start_millis}` 형식의 고유 ID.
    pub scene_id: String,
    pub schedule_id: i64,
    pub playlist_id: i64,
    pub item_id: i64,
    pub start_time_millis: i64,
    pub end_time_millis: i64,
    pub transition: Option<String>,
    pub loop_playback: bool,
    pub layout: Option<LayoutDefinition>,
    /// 이 장면 표출에 필요한 에셋 목록.
    pub asset_refs: Vec<AssetRef>,
}

impl PlaybackScene {
    /// 장면에 영상 요소가 포함되어 있는지 판별한다.
    ///
    /// 레이아웃 요소 type이 `"video"`이거나, 요소가 참조하는 에셋의
    /// mime type이 `video/*`이면 영상 장면으로 본다.
    /// 영상 장면은 switch 전에 preroll(첫 프레임 준비)이 필요하다.
    pub fn has_video(&self) -> bool {
        self.layout.as_ref().is_some_and(|layout| {
            layout.elements.iter().any(|el| {
                el.element_type == "video"
                    || self.asset_refs.iter().any(|asset| {
                        el.file_id == Some(asset.file_id)
                            && asset
                                .mime_type
                                .as_ref()
                                .is_some_and(|m| m.starts_with("video/"))
                    })
            })
        })
    }

    /// 장면 표출 시간(ms)을 반환한다.
    pub fn duration_ms(&self) -> i64 {
        self.end_time_millis - self.start_time_millis
    }
}

/// 시간순으로 정렬된 장면 목록을 가진 재생 타임라인.
///
/// `scenes`는 `start_time_millis` 오름차순 정렬이 보장된다
/// (빌더가 정렬해서 생성). 조회 함수들은 이 정렬을 전제로 이진 탐색한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackTimeline {
    pub device_id: String,
    pub date: String,
    pub revision: String,
    pub server_time: Option<String>,
    pub generated_at: Option<String>,
    pub timezone: String,
    pub scenes: Vec<PlaybackScene>,
}

impl PlaybackTimeline {
    /// `now_millis` 시각에 표출 중이어야 할 장면의 인덱스를 이진 탐색으로 찾는다.
    /// 해당 시각에 재생할 장면이 없으면 `None`.
    pub fn find_scene_index(&self, now_millis: i64) -> Option<usize> {
        let mut low = 0usize;
        let mut high = self.scenes.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let scene = &self.scenes[mid];
            if now_millis < scene.start_time_millis {
                high = mid;
            } else if now_millis >= scene.end_time_millis {
                low = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    /// 현재 시각에 표출 중이어야 할 장면을 반환한다.
    pub fn current_scene(&self, now_millis: i64) -> Option<&PlaybackScene> {
        self.find_scene_index(now_millis)
            .map(|idx| &self.scenes[idx])
    }

    /// 다음에 표출할 장면을 반환한다.
    ///
    /// - 현재 장면이 있으면: 그 바로 다음 장면
    /// - 현재 장면이 없고 첫 장면이 미래이면: 첫 장면
    /// - 그 외(모든 장면이 과거): `None`
    pub fn next_scene(&self, now_millis: i64) -> Option<&PlaybackScene> {
        match self.find_scene_index(now_millis) {
            Some(idx) => self.scenes.get(idx + 1),
            None if self.scenes.is_empty() => None,
            None if now_millis < self.scenes[0].start_time_millis => Some(&self.scenes[0]),
            None => None,
        }
    }

    /// `[start_ms, end_ms)` 구간과 겹치는 모든 장면을 반환한다.
    /// 에셋 선다운로드 window 계산에 사용한다.
    pub fn scenes_in_window(&self, start_ms: i64, end_ms: i64) -> Vec<&PlaybackScene> {
        self.scenes
            .iter()
            .filter(|s| s.end_time_millis > start_ms && s.start_time_millis < end_ms)
            .collect()
    }

    /// 타임라인 전체에서 중복 제거된 에셋 목록을 반환한다.
    /// 백그라운드 다운로드 대상 계산에 사용한다.
    pub fn all_asset_refs(&self) -> Vec<AssetRef> {
        let mut refs: Vec<AssetRef> = self
            .scenes
            .iter()
            .flat_map(|s| s.asset_refs.clone())
            .collect();
        refs.sort_by(|a, b| a.cache_key().cmp(&b.cache_key()));
        refs.dedup_by(|a, b| a.cache_key() == b.cache_key());
        refs
    }
}
