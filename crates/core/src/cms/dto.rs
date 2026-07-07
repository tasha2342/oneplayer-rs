//! CMS API 응답 DTO 정의 (serde).
//!
//! 필드명은 CMS JSON의 snake_case를 그대로 따르고,
//! Rust 예약어(`loop`, `type`, `override`)만 rename으로 처리한다.
//! 미래에 추가될 필드에 대비해 알 수 없는 키는 무시한다
//! (serde 기본 동작 + `#[serde(default)]`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `GET /play_data` 응답 최상위 래퍼.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackDataResponse {
    pub message: String,
    pub data: PlaybackDataDto,
}

/// 재생 데이터 본체 (하루치 스케줄).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackDataDto {
    #[serde(rename = "device_id")]
    pub device_id: String,
    /// 스케줄 기준 날짜 (`YYYY-MM-DD`).
    pub date: String,
    pub revision: String,
    #[serde(rename = "contact_ip")]
    pub contact_ip: Option<String>,
    /// 서버 시각 (클럭 보조 보정에 사용).
    #[serde(rename = "server_time")]
    pub server_time: Option<String>,
    #[serde(rename = "generated_at")]
    pub generated_at: Option<String>,
    /// 타임존 (기본 Asia/Seoul).
    pub timezone: Option<String>,
    /// 시간대별 재생 슬롯 목록.
    pub slots: Vec<PlaybackSlotDto>,
    /// 최상위 에셋 메타데이터 목록.
    #[serde(default)]
    pub assets: Vec<PlaybackAssetDto>,
    /// 긴급 편성 등 override 데이터 (v1에서는 파싱만).
    #[serde(default, rename = "override")]
    pub override_data: Option<Value>,
}

/// 에셋(파일) 메타데이터.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackAssetDto {
    #[serde(rename = "file_id")]
    pub file_id: i64,
    pub revision: Option<String>,
    #[serde(rename = "download_url")]
    pub download_url: String,
    #[serde(rename = "mime_type")]
    pub mime_type: Option<String>,
    /// 파일 크기 (다운로드 검증에 사용).
    #[serde(rename = "size_bytes")]
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
}

/// 시간대별 재생 슬롯 (시작~종료 시각 + 재생 item 목록).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackSlotDto {
    /// 시작 시각 (`HH:MM:SS`, 슬롯 날짜 기준).
    #[serde(rename = "start_time")]
    pub start_time: String,
    /// 종료 시각 (`HH:MM:SS`). 시작보다 빠르면 자정을 넘는 슬롯.
    #[serde(rename = "end_time")]
    pub end_time: String,
    #[serde(rename = "schedule_id")]
    pub schedule_id: i64,
    #[serde(rename = "playlist_id")]
    pub playlist_id: i64,
    #[serde(rename = "playlist_name")]
    pub playlist_name: Option<String>,
    /// position 순으로 반복 재생되는 item 목록.
    pub items: Vec<PlaybackItemDto>,
}

/// 재생 item (하나의 콘텐츠 단위).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackItemDto {
    pub id: i64,
    #[serde(rename = "playlist_id")]
    pub playlist_id: i64,
    /// slot 안에서의 재생 순서.
    pub position: i32,
    #[serde(rename = "item_type")]
    pub item_type: String,
    #[serde(rename = "ref_id")]
    pub ref_id: i64,
    /// 재생 시간(초). 0이면 playback_data/layout 기본값 사용.
    #[serde(rename = "duration_seconds", default)]
    pub duration_seconds: i64,
    pub transition: Option<String>,
    /// `loop`는 Rust 예약어라 필드명을 바꾸고 rename으로 매핑.
    #[serde(rename = "loop", default)]
    pub loop_playback: bool,
    pub layout: Option<LayoutDto>,
    #[serde(rename = "playback_data")]
    pub playback_data: Option<PlaybackItemDataDto>,
    #[serde(rename = "playback_data_b64")]
    pub playback_data_b64: Option<String>,
    /// 이 item에 필요한 파일 다운로드 정보.
    #[serde(rename = "file_downloads", default)]
    pub file_downloads: Vec<FileDownloadDto>,
}

impl PlaybackItemDto {
    /// item 자체 또는 playback_data의 loop 플래그를 종합해 반복 여부를 반환한다.
    pub fn loop_enabled(&self) -> bool {
        self.loop_playback
            || self
                .playback_data
                .as_ref()
                .and_then(|d| d.loop_playback)
                .unwrap_or(false)
    }
}

/// item에 내장된 세부 재생 설정.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackItemDataDto {
    pub duration: Option<i64>,
    pub transition: Option<String>,
    #[serde(rename = "loop")]
    pub loop_playback: Option<bool>,
}

/// 파일 다운로드 정보 (item 단위 에셋 참조).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileDownloadDto {
    #[serde(rename = "file_id")]
    pub file_id: i64,
    #[serde(rename = "download_url")]
    pub download_url: String,
    pub revision: Option<String>,
    #[serde(rename = "mime_type")]
    pub mime_type: Option<String>,
    #[serde(rename = "size_bytes")]
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
}

/// 레이아웃 정의 (화면 구성).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LayoutDto {
    pub id: i64,
    pub name: String,
    #[serde(rename = "group_name")]
    pub group_name: Option<String>,
    /// 레이아웃 기준 해상도.
    pub width: i32,
    pub height: i32,
    /// 레이아웃 안의 요소 목록 (JSON 키는 `layout`).
    pub layout: Vec<LayoutElementDto>,
    #[serde(rename = "default_duration")]
    pub default_duration: Option<i64>,
}

/// 레이아웃 요소 (이미지/영상/텍스트 등).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LayoutElementDto {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// `type`은 Rust 예약어라 rename 처리.
    #[serde(rename = "type")]
    pub element_type: String,
    #[serde(default)]
    pub lock: bool,
    #[serde(rename = "keep_aspect_ratio", default)]
    pub keep_aspect_ratio: bool,
    #[serde(rename = "file_id")]
    pub file_id: Option<i64>,
    #[serde(rename = "content_group_id")]
    pub content_group_id: Option<i64>,
    #[serde(rename = "content_id")]
    pub content_id: Option<i64>,
    #[serde(rename = "duration_type")]
    pub duration_type: Option<String>,
    #[serde(rename = "url_address")]
    pub url_address: Option<String>,
    /// 텍스트 요소의 내용.
    pub content: Option<String>,
    pub font: Option<String>,
    #[serde(rename = "font_size")]
    pub font_size: Option<i32>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(rename = "background_color")]
    pub background_color: Option<String>,
    #[serde(rename = "text_color")]
    pub text_color: Option<String>,
    #[serde(rename = "border_color")]
    pub border_color: Option<String>,
    #[serde(rename = "border_width")]
    pub border_width: Option<i32>,
    #[serde(rename = "z_index")]
    pub z_index: Option<i32>,
}
