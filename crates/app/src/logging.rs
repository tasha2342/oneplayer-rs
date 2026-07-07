//! tracing 로거 초기화 (콘솔 + 일 단위 파일 롤링).
//!
//! 파일 로그는 `{data_dir}/logs/oneplayer.log.YYYY-MM-DD`로 기록된다.
//! v2 debug overlay가 표시할 지표(delay, preroll lead 등)는
//! 이 로그에 이미 기록되고 있으므로 overlay는 표시만 추가하면 된다.

use anyhow::Result;
use oneplayer_core::settings::AppSettings;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 로거를 초기화한다. 프로세스당 한 번만 호출해야 한다.
pub fn init(settings: &AppSettings) -> Result<()> {
    std::fs::create_dir_all(settings.logs_dir())?;
    let file_appender = tracing_appender::rolling::daily(settings.logs_dir(), "oneplayer.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // guard가 drop되면 파일 로그가 멈추므로 프로세스 수명 동안 유지시킨다.
    std::mem::forget(guard);

    // RUST_LOG 환경변수로 레벨 조정 가능. 기본은 info.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
        .init();
    Ok(())
}
