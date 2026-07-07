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
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use tracing::info;

use crate::settings::AppSettings;

/// CMS HTTP API 클라이언트.
pub struct CmsApiClient {
    client: Client,
    /// 끝 슬래시가 제거된 base URL.
    base_url: String,
    /// Bearer 인증 토큰 (없으면 헤더 생략).
    auth_token: Option<String>,
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
    pub async fn get_playback_data(
        &self,
        device_id: &str,
        date: &str,
    ) -> Result<PlaybackDataDto> {
        let url = format!(
            "{}/v1/playback/play_data?device_id={}&date={}",
            self.api_root(),
            urlencoding::encode(device_id),
            urlencoding::encode(date)
        );
        info!(%url, "fetching playback data");
        let response: PlaybackDataResponse = self.get_json(&url).await?;
        Ok(response.data)
    }

    /// 마지막 성공 play_data를 오프라인 부팅용으로 JSON 파일에 저장한다.
    pub fn save_playback_cache(path: &Path, data: &PlaybackDataDto) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(data)?)?;
        Ok(())
    }

    /// 저장된 play_data 캐시를 읽는다. 파일이 없으면 `Ok(None)`.
    pub fn load_playback_cache(path: &Path) -> Result<Option<PlaybackDataDto>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        let data = serde_json::from_str(&raw)?;
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
        let mut req = self.client.get(url);
        if let Some(token) = &self.auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = req.send().await.context("CMS request failed")?;
        let status = response.status();
        let body = response.text().await.context("failed to read CMS body")?;
        if !status.is_success() {
            anyhow::bail!("CMS request failed: {status} {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("failed to parse CMS JSON from {url}"))
    }
}
