//! 로컬 에셋 캐시.
//!
//! 정책 (OnePlayer 0.4.0 계승):
//! - 저장 위치: `{data_dir}/assets/{fileId}_{revision}.bin`
//! - 다운로드 중 임시 파일 `.part` → 완료 후 atomic rename
//! - `size_bytes`가 있으면 파일 크기로, checksum이 있으면 SHA-256으로 검증
//! - 캐시 정리: stale `.part` 삭제 + 보호 window 밖 파일 삭제 + 총량 상한(1GB) LRU
//!
//! 파일 구성:
//! - `mod.rs`: [`AssetStore`] 본체 (준비 확인, 다운로드)
//! - [`cleanup`]: 캐시 정리 로직
//! - [`fsutil`]: 파일 시스템 헬퍼 (수정 시각, 해시 등)

mod cleanup;
mod fsutil;

pub use cleanup::CacheCleanupResult;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use reqwest::Client;
use tracing::{debug, error, info};

use crate::settings::AppSettings;
use crate::timeline::AssetRef;

use fsutil::{normalize_checksum, sha256_file, touch_file};

/// 에셋 파일의 다운로드와 로컬 캐시 관리를 담당한다.
pub struct AssetStore {
    /// 에셋 저장 디렉터리 (`{data_dir}/assets`).
    asset_dir: PathBuf,
    /// 다운로드용 HTTP 클라이언트.
    client: Client,
    /// 상대 경로 download_url을 절대 URL로 만들 때 쓰는 CMS base URL.
    cms_base_url: String,
    /// CMS 인증 토큰 (없으면 헤더 생략).
    auth_token: Option<String>,
    /// 캐시 총량 상한 (기본 1GB).
    pub(crate) max_cache_size_bytes: u64,
}

impl AssetStore {
    /// 설정 기반으로 캐시 저장소를 생성한다. 저장 디렉터리가 없으면 만든다.
    pub fn new(settings: &AppSettings) -> Result<Self> {
        let asset_dir = settings.assets_dir();
        fs::create_dir_all(&asset_dir)?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        let auth_token = if settings.auth_token.trim().is_empty() {
            None
        } else {
            Some(settings.auth_token.clone())
        };
        Ok(Self {
            asset_dir,
            client,
            cms_base_url: settings.cms_base_url.trim_end_matches('/').to_string(),
            auth_token,
            max_cache_size_bytes: settings.max_cache_size_bytes(),
        })
    }

    /// 에셋 저장 디렉터리 경로를 반환한다.
    pub fn asset_dir(&self) -> &Path {
        &self.asset_dir
    }

    /// 에셋의 로컬 파일 경로(`{cache_key}.bin`)를 계산한다.
    pub fn local_path(&self, asset: &AssetRef) -> PathBuf {
        self.asset_dir.join(format!("{}.bin", asset.cache_key()))
    }

    /// 에셋이 로컬에 완전하게 준비되어 있는지 검증한다.
    ///
    /// 검증 우선순위:
    /// 1. `size_bytes`가 있으면 → 파일 크기 일치 확인
    /// 2. checksum이 있으면 → SHA-256 일치 확인
    /// 3. 둘 다 없으면 → 다운로드 완료 marker(`.complete`) 존재 확인
    pub fn is_ready(&self, asset: &AssetRef) -> bool {
        let file = self.local_path(asset);
        if !file.exists() {
            return false;
        }
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            return false;
        }
        if let Some(expected) = asset.size_bytes {
            return len == expected as u64;
        }
        if let Some(ref checksum) = asset.checksum {
            if let Ok(actual) = sha256_file(&file) {
                return normalize_checksum(checksum) == actual;
            }
            return false;
        }
        self.marker_path(asset).exists()
    }

    /// 모든 에셋이 준비됐는지 확인한다. 하나라도 없으면
    /// 누락 목록(최대 5개)을 담은 에러를 반환한다.
    ///
    /// 오프라인 캐시 재생 시작 전 검증에 사용된다
    /// (표출 시점 다운로드 금지 정책).
    pub fn verify_all_ready(&self, assets: &[AssetRef]) -> Result<()> {
        let missing: Vec<_> = assets
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|a| !self.is_ready(a))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "assets not ready: {}",
                missing
                    .iter()
                    .take(5)
                    .map(|a| format!("{}/{}", a.file_id, a.cache_key()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }

    /// 에셋 목록을 순서대로 준비(없으면 다운로드)하고
    /// cache_key → 로컬 경로 매핑을 반환한다.
    ///
    /// 다운로드 후에도 검증에 실패하면 에러를 반환한다
    /// (손상 파일로 재생하지 않기 위함).
    pub async fn ensure_assets(&self, assets: &[AssetRef]) -> Result<HashMap<String, PathBuf>> {
        let mut result = HashMap::new();
        for asset in assets {
            let key = asset.cache_key();
            // 같은 목록 안의 중복 에셋은 한 번만 처리한다.
            if result.contains_key(&key) {
                continue;
            }
            let path = self.local_path(asset);
            if !self.is_ready(asset) {
                let source = asset_source(asset);
                info!(
                    api_stage = "asset_download_started",
                    source,
                    file_id = asset.file_id,
                    cache_key = %key,
                    mime_type = asset.mime_type.as_deref().unwrap_or(""),
                    expected_size_bytes = ?asset.size_bytes,
                    url = %url_without_query(&asset.download_url),
                    "asset download started"
                );
                if let Err(err) = self.download(asset, &path).await {
                    error!(
                        api_stage = "asset_download_failed",
                        source,
                        file_id = asset.file_id,
                        cache_key = %key,
                        mime_type = asset.mime_type.as_deref().unwrap_or(""),
                        expected_size_bytes = ?asset.size_bytes,
                        url = %url_without_query(&asset.download_url),
                        error = %format!("{err:#}"),
                        "asset download failed"
                    );
                    return Err(err);
                }
            } else {
                debug!(
                    api_stage = "asset_cache_hit",
                    source = asset_source(asset),
                    file_id = asset.file_id,
                    cache_key = %key,
                    "asset already available in cache"
                );
            }
            if !self.is_ready(asset) {
                error!(
                    api_stage = "asset_verification_failed",
                    source = asset_source(asset),
                    file_id = asset.file_id,
                    cache_key = %key,
                    local_path = %path.display(),
                    expected_size_bytes = ?asset.size_bytes,
                    has_checksum = asset.checksum.is_some(),
                    "asset verification failed after download"
                );
                anyhow::bail!(
                    "asset verification failed after download file_id={} key={}",
                    asset.file_id,
                    key
                );
            }
            // 접근 시각을 갱신해 LRU 정리에서 뒤로 밀리게 한다.
            touch_file(&path)?;
            touch_file(&self.marker_path(asset))?;
            debug!(
                api_stage = "asset_ready",
                source = asset_source(asset),
                file_id = asset.file_id,
                cache_key = %key,
                local_path = %path.display(),
                "asset ready"
            );
            result.insert(key, path);
        }
        Ok(result)
    }

    /// 에셋 하나를 다운로드한다.
    ///
    /// 순서: `.part` 임시 파일에 기록 → fsync → 크기 검증
    /// → atomic rename으로 최종 파일 교체 → `.complete` marker 생성.
    /// 중간에 실패해도 최종 파일은 항상 완전한 상태만 존재한다.
    async fn download(&self, asset: &AssetRef, destination: &Path) -> Result<()> {
        let started = Instant::now();
        let temp = destination.with_extension("part");
        if let Some(parent) = temp.parent() {
            fs::create_dir_all(parent)?;
        }

        // HTTP 요청 (인증 토큰이 있으면 Bearer 헤더 추가).
        let url = resolve_url(&self.cms_base_url, &asset.download_url);
        let mut req = self.client.get(&url);
        if let Some(token) = &self.auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = req.send().await.context("asset download request failed")?;
        let status = response.status();
        if !status.is_success() {
            error!(
                api_stage = "asset_http_status_failed",
                source = asset_source(asset),
                file_id = asset.file_id,
                cache_key = %asset.cache_key(),
                url = %url_without_query(&url),
                status = %status,
                elapsed_ms = started.elapsed().as_millis(),
                "asset server returned failure status"
            );
            anyhow::bail!("asset download failed: {} {}", status, asset.file_id);
        }

        // 임시 파일에 쓰고 디스크에 확실히 반영(fsync)한다.
        let bytes = response.bytes().await?;
        let received_size = bytes.len();
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }

        // 크기가 기대값과 다르면 손상으로 보고 임시 파일을 버린다.
        if let Some(expected) = asset.size_bytes {
            let actual = temp.metadata()?.len();
            if actual != expected as u64 {
                let _ = fs::remove_file(&temp);
                error!(
                    api_stage = "asset_size_mismatch",
                    source = asset_source(asset),
                    file_id = asset.file_id,
                    cache_key = %asset.cache_key(),
                    expected_size_bytes = expected,
                    actual_size_bytes = actual,
                    elapsed_ms = started.elapsed().as_millis(),
                    "downloaded asset size does not match metadata"
                );
                anyhow::bail!(
                    "asset size mismatch file_id={} expected={} actual={}",
                    asset.file_id,
                    expected,
                    actual
                );
            }
        }

        // atomic rename: 이 시점부터 최종 파일이 완전한 상태로 존재한다.
        fs::rename(&temp, destination)?;
        fs::write(self.marker_path(asset), b"ok")?;
        info!(
            api_stage = "asset_download_completed",
            source = asset_source(asset),
            file_id = asset.file_id,
            cache_key = %asset.cache_key(),
            received_size_bytes = received_size,
            local_path = %destination.display(),
            elapsed_ms = started.elapsed().as_millis(),
            "asset download completed"
        );
        Ok(())
    }

    /// 다운로드 완료 marker 파일(`{cache_key}.complete`) 경로를 계산한다.
    /// size/checksum이 없는 에셋의 준비 여부 판단에 사용한다.
    pub(crate) fn marker_path(&self, asset: &AssetRef) -> PathBuf {
        self.asset_dir
            .join(format!("{}.complete", asset.cache_key()))
    }
}

/// download_url이 상대 경로면 CMS base URL과 결합해 절대 URL로 만든다.
///
/// play_data의 `download_url`이 `/api/v1/...`처럼 절대 경로로 오면
/// `cms_base_url`의 호스트(origin)에 붙인다 (`/api` 중복 방지).
fn resolve_url(base_url: &str, download_url: &str) -> String {
    if download_url.starts_with("http://") || download_url.starts_with("https://") {
        return download_url.to_string();
    }
    if download_url.starts_with('/') {
        return format!("{}{download_url}", cms_origin(base_url));
    }
    let base = base_url.trim_end_matches('/');
    let path = download_url.trim_start_matches('/');
    format!("{base}/{path}")
}

fn asset_source(asset: &AssetRef) -> &'static str {
    if asset.file_id < 0 {
        "rtb"
    } else {
        "cms"
    }
}

fn url_without_query(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// `cms_base_url`에서 호스트 origin만 추출한다 (`.../api` 접미사 제거).
fn cms_origin(base_url: &str) -> &str {
    let trimmed = base_url.trim_end_matches('/');
    trimmed.strip_suffix("/api").unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::{cms_origin, resolve_url};

    #[test]
    fn resolve_absolute_path_download_url() {
        assert_eq!(
            resolve_url(
                "https://kn.jdone.co.kr/api",
                "/api/v1/content-files/38/file"
            ),
            "https://kn.jdone.co.kr/api/v1/content-files/38/file"
        );
    }

    #[test]
    fn resolve_relative_download_url() {
        assert_eq!(
            resolve_url("https://kn.jdone.co.kr/api", "v1/content-files/38/file"),
            "https://kn.jdone.co.kr/api/v1/content-files/38/file"
        );
    }

    #[test]
    fn resolve_absolute_http_url_unchanged() {
        assert_eq!(
            resolve_url(
                "https://kn.jdone.co.kr/api",
                "https://cdn.example.com/file.bin"
            ),
            "https://cdn.example.com/file.bin"
        );
    }

    #[test]
    fn cms_origin_strips_api_suffix() {
        assert_eq!(
            cms_origin("https://kn.jdone.co.kr/api"),
            "https://kn.jdone.co.kr"
        );
        assert_eq!(
            cms_origin("https://kn.jdone.co.kr"),
            "https://kn.jdone.co.kr"
        );
    }
}
