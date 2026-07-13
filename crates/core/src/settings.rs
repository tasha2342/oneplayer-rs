//! 앱 설정 (`config.toml`) 로드/저장.
//!
//! Android OnePlayer의 SettingsRepository에 해당한다.
//! v1.1부터 앱 내 설정 UI로 일부 항목을 편집할 수 있다.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::DEFAULT_MAX_CACHE_SIZE_BYTES;

/// 앱 전체 설정. `config.toml`과 1:1로 매핑된다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// 단말 식별자 (CMS 조회 키).
    pub device_id: String,
    /// CMS base URL (`/api` 포함/미포함 모두 허용).
    pub cms_base_url: String,
    /// CMS 인증 토큰 (빈 문자열이면 인증 헤더 생략).
    #[serde(default)]
    pub auth_token: String,
    /// NTP 서버 주소.
    pub ntp_server: String,
    /// 데이터 저장 경로 (기본: `%LOCALAPPDATA%/OnePlayer`).
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// 스케줄 동기화 주기 (초, 기본 300 = 5분).
    #[serde(default = "default_sync_interval")]
    pub schedule_sync_interval_sec: u64,
    /// 에셋 선다운로드 window (분, 기본 5).
    #[serde(default = "default_preload_minutes")]
    pub asset_preload_minutes: u64,
    /// 캐시 총량 상한 (MB, 기본 1024 = 1GB).
    #[serde(default = "default_max_cache_mb")]
    pub max_cache_size_mb: u64,
    /// 캔버스(창) 가로 해상도.
    #[serde(default = "default_canvas_width")]
    pub canvas_width: u32,
    /// 캔버스(창) 세로 해상도. 세로형 DID면 1920.
    #[serde(default = "default_canvas_height")]
    pub canvas_height: u32,
    /// 전체화면(borderless) 여부.
    #[serde(default = "default_true")]
    pub fullscreen: bool,
    /// 전환 실패 시 마지막 fallback으로 표시할 이미지 경로.
    #[serde(default)]
    pub fallback_image_path: Option<PathBuf>,
    /// FFmpeg 하드웨어 디코딩 (`none`=CPU, Windows 기본 `d3d11va`).
    /// 지원: `cuda`, `d3d11va`, `d3d12va`, `dxva2`, `qsv`, `vaapi`, `vulkan` 등.
    #[serde(default = "default_ffmpeg_hwaccel")]
    pub ffmpeg_hwaccel: String,
    /// 설정 버튼을 투명하게 처리한다 (닫힌 상태에서 버튼이 보이지 않음).
    #[serde(default)]
    pub settings_button_transparent: bool,
}

// ---- serde 기본값 함수들 (TOML에 항목이 없을 때 사용) ----

fn default_sync_interval() -> u64 {
    300
}

fn default_preload_minutes() -> u64 {
    5
}

fn default_max_cache_mb() -> u64 {
    1024
}

fn default_canvas_width() -> u32 {
    1080
}

fn default_canvas_height() -> u32 {
    1920
}

fn default_true() -> bool {
    true
}

fn default_ffmpeg_hwaccel() -> String {
    if cfg!(windows) {
        "d3d11va".into()
    } else {
        String::new()
    }
}

impl Default for AppSettings {
    /// 기존 Android 앱과 동일한 기본값으로 생성한다.
    fn default() -> Self {
        Self {
            device_id: "DV-1001".into(),
            cms_base_url: "https://kn.jdone.co.kr/api".into(),
            auth_token: String::new(),
            ntp_server: "101.79.18.207".into(),
            data_dir: None,
            schedule_sync_interval_sec: default_sync_interval(),
            asset_preload_minutes: default_preload_minutes(),
            max_cache_size_mb: default_max_cache_mb(),
            canvas_width: default_canvas_width(),
            canvas_height: default_canvas_height(),
            fullscreen: true,
            fallback_image_path: None,
            ffmpeg_hwaccel: default_ffmpeg_hwaccel(),
            settings_button_transparent: false,
        }
    }
}

impl AppSettings {
    /// TOML 파일에서 설정을 읽고 검증한다.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let mut settings: Self = toml::from_str(&raw).context("failed to parse config.toml")?;
        settings.apply_env_overrides();
        settings.validate()?;
        Ok(settings)
    }

    /// 환경변수 기반 런타임 override를 적용한다.
    pub fn apply_env_overrides(&mut self) {
        self.apply_device_id_override(std::env::var("ONEPLAYER_DEVICE_ID").ok());
    }

    /// 설정을 TOML 파일로 저장한다 (최초 실행 시 기본 config 생성용).
    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).context("failed to serialize config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, raw)?;
        Ok(())
    }

    /// 필수 항목(device_id, cms_base_url, ntp_server)이 비어있지 않은지 검증한다.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.device_id.trim().is_empty(), "device_id is required");
        anyhow::ensure!(
            !self.cms_base_url.trim().is_empty(),
            "cms_base_url is required"
        );
        anyhow::ensure!(!self.ntp_server.trim().is_empty(), "ntp_server is required");
        Ok(())
    }

    fn apply_device_id_override(&mut self, value: Option<String>) {
        if let Some(device_id) = value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            self.device_id = device_id;
        }
    }

    /// 데이터 저장 루트 디렉터리를 결정한다.
    /// 설정값이 있으면 환경변수 확장 후 사용, 없으면 OS 표준 위치.
    pub fn resolve_data_dir(&self) -> PathBuf {
        if let Some(dir) = &self.data_dir {
            return expand_env_path(dir);
        }
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("OnePlayer")
    }

    /// 에셋 캐시 디렉터리 경로.
    pub fn assets_dir(&self) -> PathBuf {
        self.resolve_data_dir().join("assets")
    }

    /// 오프라인 재생용 play_data 캐시 파일 경로.
    pub fn playback_cache_path(&self) -> PathBuf {
        self.resolve_data_dir().join("playback_cache.json")
    }

    /// 클럭 보정 상태 저장 파일 경로.
    pub fn clock_state_path(&self) -> PathBuf {
        self.resolve_data_dir().join("clock_state.json")
    }

    /// 로그 디렉터리 경로.
    pub fn logs_dir(&self) -> PathBuf {
        self.resolve_data_dir().join("logs")
    }

    /// 캐시 상한을 바이트 단위로 변환한다.
    pub fn max_cache_size_bytes(&self) -> u64 {
        self.max_cache_size_mb
            .saturating_mul(1024)
            .saturating_mul(1024)
            .max(DEFAULT_MAX_CACHE_SIZE_BYTES / 1024 / 1024)
    }

    /// 에셋 선다운로드 window를 밀리초로 변환한다.
    pub fn asset_preload_window_ms(&self) -> i64 {
        self.asset_preload_minutes as i64 * 60 * 1_000
    }

    /// 동기화 주기를 밀리초로 변환한다.
    pub fn sync_interval_ms(&self) -> i64 {
        self.schedule_sync_interval_sec as i64 * 1_000
    }
}

/// 경로 문자열의 `%LOCALAPPDATA%`를 실제 경로로 치환한다.
fn expand_env_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.contains("%LOCALAPPDATA%") {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .to_string();
        return PathBuf::from(s.replace("%LOCALAPPDATA%", &base));
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 기본 설정이 검증을 통과하는지 확인한다.
    #[test]
    fn default_settings_validate() {
        AppSettings::default().validate().unwrap();
    }

    #[test]
    fn device_id_env_override_uses_non_empty_value() {
        let mut settings = AppSettings::default();
        settings.apply_device_id_override(Some(" DEVICE-ENV ".into()));
        assert_eq!(settings.device_id, "DEVICE-ENV");
    }

    #[test]
    fn device_id_env_override_ignores_blank_value() {
        let mut settings = AppSettings::default();
        let original = settings.device_id.clone();
        settings.apply_device_id_override(Some("  ".into()));
        assert_eq!(settings.device_id, original);
    }
}
