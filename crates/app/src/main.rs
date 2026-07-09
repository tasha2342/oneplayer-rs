//! OnePlayer 앱 진입점.
//!
//! 역할 분담:
//! - `main.rs`: 설정 로드, 로깅 초기화, winit 이벤트 루프 구동
//! - [`app`]: 엔진/렌더러를 조립하는 애플리케이션 상태
//! - [`logging`]: tracing 로거 (콘솔 + 일 단위 파일 롤링)
//! - [`sample`]: `--sample` 데모 스케줄 (Android Phase 1 최소 동작 예제)
//! - [`windows_power`]: Windows 절전/화면꺼짐 방지

use std::path::PathBuf;

use anyhow::{Context, Result};
use oneplayer_core::settings::AppSettings;
use tracing::info;
use winit::event_loop::EventLoop;

mod app;
mod logging;
mod sample;
mod settings_ui;
#[cfg(windows)]
mod windows_power;

use app::App;

/// 실행 인자에서 config 경로를 읽는다. 없으면 `config.toml`.
fn config_path() -> PathBuf {
    std::env::args()
        .nth(1)
        .filter(|a| a != "--sample")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// 설정 파일을 로드한다.
/// 파일이 없거나 파싱에 실패하면 기본값을 사용하고,
/// 파일이 아예 없으면 기본 config.toml을 생성해 둔다 (최초 실행 편의).
fn load_settings() -> AppSettings {
    let path = config_path();
    AppSettings::load(&path).unwrap_or_else(|err| {
        eprintln!("config load failed ({err:#}), using defaults");
        let default = AppSettings::default();
        if !path.exists() {
            let _ = default.save(&path);
        }
        default
    })
}

/// 프로세스 진입점.
/// 실패 시 non-zero exit code를 보장한다
/// (v2 watchdog이 재시작 신호로 사용).
fn main() -> Result<()> {
    let result = run();
    if result.is_err() {
        std::process::exit(1);
    }
    result
}

/// 앱 초기화와 이벤트 루프 실행.
fn run() -> Result<()> {
    let sample_mode = std::env::args().any(|a| a == "--sample");
    let config_path = config_path();
    let settings = load_settings();
    logging::init(&settings)?;
    info!(
        device_id = %settings.device_id,
        cms = %settings.cms_base_url,
        sample_mode,
        "OnePlayer starting"
    );

    // winit 이벤트 루프가 메인 스레드를 점유한다.
    // 엔진은 App 내부의 tokio 런타임에서 백그라운드로 돈다.
    let event_loop = EventLoop::new().context("event loop")?;
    let mut app = App::new(settings, config_path, sample_mode)?;
    event_loop.run_app(&mut app).context("event loop run")?;
    Ok(())
}
