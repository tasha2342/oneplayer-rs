//! tracing 로거 초기화 (파일 + 디버그 빌드 시 콘솔).
//!
//! 파일 로그는 `{data_dir}/logs/oneplayer.log.YYYY-MM-DD`로 기록된다.
//! `tracing-appender::non_blocking`으로 쓰기를 백그라운드 스레드에 넘기므로
//! 재생 루프 등 핫 패스에는 거의 영향이 없다.

use anyhow::Result;
use oneplayer_core::settings::AppSettings;
use tracing::info;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 로거를 초기화한다. 프로세스당 한 번만 호출해야 한다.
pub fn init(settings: &AppSettings) -> Result<()> {
    let logs_dir = settings.logs_dir();
    std::fs::create_dir_all(&logs_dir)?;
    oneplayer_core::timing_log::init(&logs_dir)?;
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "oneplayer.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // guard가 drop되면 파일 로그가 멈추므로 프로세스 수명 동안 유지시킨다.
    std::mem::forget(guard);

    // RUST_LOG 환경변수로 레벨 조정 가능. 기본은 info.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);

    // 릴리스 빌드는 windows_subsystem으로 콘솔이 없으므로 파일만 기록한다.
    #[cfg(debug_assertions)]
    {
        registry
            .with(fmt::layer().with_writer(std::io::stdout))
            .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
            .init();
    }
    #[cfg(not(debug_assertions))]
    {
        registry
            .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
            .init();
    }

    info!(logs_dir = %logs_dir.display(), "file logging enabled");
    Ok(())
}
