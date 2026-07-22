//! CMS API 연동.
//!
//! 기존 Android OnePlayer와 동일한 엔드포인트를 사용한다:
//! - `GET /api/v1/playback/play_data?device_id=&date=` — 전체 스케줄/에셋 데이터
//!
//! 응답의 `revision`이 이전과 같으면 타임라인 재구성을 생략한다.
//! 마지막 성공 응답은 JSON 파일로 캐시해 오프라인 부팅에 사용한다.

mod dto;

pub use dto::*;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::settings::AppSettings;

static CMS_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// CMS HTTP API 클라이언트.
#[derive(Clone)]
pub struct CmsApiClient {
    client: Client,
    /// 끝 슬래시가 제거된 base URL.
    base_url: String,
    /// Bearer 인증 토큰 (없으면 헤더 생략).
    auth_token: Option<String>,
}

/// 재생 완료 로그 한 건.
#[derive(Debug, Clone, Serialize)]
pub struct PlaybackLogItem {
    pub device_id: String,
    pub content_type: String,
    pub content_id: i64,
    pub started_at: String,
    pub ended_at: String,
    pub completed: bool,
    pub extra: Value,
}

#[derive(Debug, Serialize)]
struct PlaybackLogBatchRequest<'a> {
    items: &'a [PlaybackLogItem],
}

impl CmsApiClient {
    /// 설정 기반으로 클라이언트를 생성한다 (요청 타임아웃 30초).
    pub fn new(settings: &AppSettings) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;
        let auth_token = if settings.auth_token.trim().is_empty() {
            None
        } else {
            Some(settings.auth_token.clone())
        };
        Ok(Self {
            client,
            base_url: settings.cms_base_url.trim_end_matches('/').to_string(),
            auth_token,
        })
    }

    /// 전체 재생 데이터(슬롯/아이템/에셋)를 조회한다.
    ///
    /// `date`는 `YYYY-MM-DD` 형식 (스케줄 기준일).
    pub async fn get_playback_data(&self, device_id: &str, date: &str) -> Result<PlaybackDataDto> {
        let url = format!(
            "{}/v1/playback/play_data?device_id={}&date={}",
            self.api_root(),
            urlencoding::encode(device_id),
            urlencoding::encode(date)
        );
        info!(
            api_stage = "play_data_request",
            device_id,
            date,
            %url,
            "CMS playback data fetch started"
        );
        let response: PlaybackDataResponse = self.get_json(&url).await?;
        info!(
            api_stage = "play_data_parsed",
            device_id,
            date,
            api_version = response.data.api_version.as_deref().unwrap_or("1.0"),
            revision = %response.data.revision,
            slot_count = response.data.slots.len(),
            rtb_slot_count = response.data.rtb_slots.len(),
            asset_count = response.data.assets.len(),
            "CMS playback data parsed"
        );
        Ok(response.data)
    }

    /// 재생 완료 로그를 배치로 전송한다.
    pub async fn post_playback_logs_batch(&self, items: &[PlaybackLogItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let url = format!("{}/v1/playback-logs/batch", self.api_root());
        self.post_json(&url, &PlaybackLogBatchRequest { items })
            .await
    }

    /// 마지막 성공 play_data를 오프라인 부팅용으로 JSON 파일에 저장한다.
    pub fn save_playback_cache(path: &Path, data: &PlaybackDataDto) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(data)?)?;
        info!(
            api_stage = "play_data_cache_saved",
            path = %path.display(),
            revision = %data.revision,
            slot_count = data.slots.len(),
            rtb_slot_count = data.rtb_slots.len(),
            "playback cache saved"
        );
        Ok(())
    }

    /// 저장된 play_data 캐시를 읽는다. 파일이 없으면 `Ok(None)`.
    pub fn load_playback_cache(path: &Path) -> Result<Option<PlaybackDataDto>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        let data: PlaybackDataDto = serde_json::from_str(&raw).map_err(|err| {
            error!(
                api_stage = "play_data_cache_parse_failed",
                path = %path.display(),
                body_bytes = raw.len(),
                error = %err,
                "failed to parse cached playback data"
            );
            err
        })?;
        info!(
            api_stage = "play_data_cache_loaded",
            path = %path.display(),
            revision = %data.revision,
            slot_count = data.slots.len(),
            rtb_slot_count = data.rtb_slots.len(),
            "playback cache loaded"
        );
        Ok(Some(data))
    }

    /// base URL이 이미 `/api`로 끝나면 그대로, 아니면 `/api`를 붙인다.
    /// (설정에 `https://host` / `https://host/api` 어느 쪽을 넣어도 동작)
    fn api_root(&self) -> String {
        if self.base_url.ends_with("/api") {
            self.base_url.clone()
        } else {
            format!("{}/api", self.base_url)
        }
    }

    /// GET 요청을 보내고 JSON을 역직렬화한다.
    /// 인증 토큰이 설정돼 있으면 `Authorization: Bearer` 헤더를 붙인다.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let request_id = CMS_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let mut req = self.client.get(url);
        if let Some(token) = &self.auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        debug!(
            api_stage = "http_request_started",
            request_id,
            method = "GET",
            %url,
            has_auth = self.auth_token.is_some(),
            "CMS HTTP request started"
        );
        let response = match req.send().await {
            Ok(response) => response,
            Err(err) => {
                error!(
                    api_stage = "http_transport_failed",
                    request_id,
                    method = "GET",
                    %url,
                    elapsed_ms = started.elapsed().as_millis(),
                    is_timeout = err.is_timeout(),
                    is_connect = err.is_connect(),
                    error = %err,
                    "CMS HTTP transport failed"
                );
                return Err(err).context("CMS request failed");
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                error!(
                    api_stage = "http_body_read_failed",
                    request_id,
                    method = "GET",
                    %url,
                    status = %status,
                    elapsed_ms = started.elapsed().as_millis(),
                    error = %err,
                    "failed to read CMS response body"
                );
                return Err(err).context("failed to read CMS body");
            }
        };
        debug!(
            api_stage = "http_response_received",
            request_id,
            method = "GET",
            %url,
            status = %status,
            body_bytes = body.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "CMS HTTP response received"
        );
        if !status.is_success() {
            error!(
                api_stage = "http_status_failed",
                request_id,
                method = "GET",
                %url,
                status = %status,
                body_preview = %body_preview(&body),
                elapsed_ms = started.elapsed().as_millis(),
                "CMS returned failure status"
            );
            anyhow::bail!("CMS request failed: {status} {body}");
        }
        serde_json::from_str(&body).map_err(|err| {
            error!(
                api_stage = "json_parse_failed",
                request_id,
                method = "GET",
                %url,
                status = %status,
                body_bytes = body.len(),
                elapsed_ms = started.elapsed().as_millis(),
                error = %err,
                "failed to parse CMS JSON"
            );
            anyhow::Error::new(err).context(format!("failed to parse CMS JSON from {url}"))
        })
    }

    /// JSON POST 요청을 보내고 성공 status만 확인한다.
    async fn post_json<T: Serialize + ?Sized>(&self, url: &str, body: &T) -> Result<()> {
        let request_id = CMS_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let mut req = self.client.post(url).json(body);
        if let Some(token) = &self.auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        debug!(
            api_stage = "http_request_started",
            request_id,
            method = "POST",
            %url,
            has_auth = self.auth_token.is_some(),
            "CMS HTTP request started"
        );
        let response = match req.send().await {
            Ok(response) => response,
            Err(err) => {
                error!(
                    api_stage = "http_transport_failed",
                    request_id,
                    method = "POST",
                    %url,
                    elapsed_ms = started.elapsed().as_millis(),
                    is_timeout = err.is_timeout(),
                    is_connect = err.is_connect(),
                    error = %err,
                    "CMS POST transport failed"
                );
                return Err(err).context("CMS POST request failed");
            }
        };
        let status = response.status();
        let text = match response.text().await {
            Ok(text) => text,
            Err(err) => {
                error!(
                    api_stage = "http_body_read_failed",
                    request_id,
                    method = "POST",
                    %url,
                    status = %status,
                    elapsed_ms = started.elapsed().as_millis(),
                    error = %err,
                    "failed to read CMS POST response body"
                );
                return Err(err).context("failed to read CMS POST body");
            }
        };
        if !status.is_success() {
            error!(
                api_stage = "http_status_failed",
                request_id,
                method = "POST",
                %url,
                status = %status,
                body_preview = %body_preview(&text),
                elapsed_ms = started.elapsed().as_millis(),
                "CMS POST returned failure status"
            );
            anyhow::bail!("CMS POST request failed: {status} {text}");
        }
        debug!(
            api_stage = "http_request_completed",
            request_id,
            method = "POST",
            %url,
            status = %status,
            body_bytes = text.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "CMS POST completed"
        );
        Ok(())
    }
}

fn body_preview(body: &str) -> String {
    const MAX_CHARS: usize = 512;
    let mut preview: String = body.chars().take(MAX_CHARS).collect();
    if body.chars().count() > MAX_CHARS {
        preview.push('…');
    }
    preview.replace(['\r', '\n'], " ")
}
