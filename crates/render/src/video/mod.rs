//! 영상 디코더 추상화와 구현.
//!
//! [`VideoDecoder`] trait 뒤에 실제 디코더를 숨겨,
//! 디코더 구현(FFmpeg CLI → Media Foundation 등) 교체가 가능하게 한다.
//!
//! 기본 구현은 [`FfmpegCliDecoder`]: `ffmpeg.exe` 서브프로세스가
//! RGBA raw 프레임을 stdout 파이프로 출력하고, 리더 스레드가 이를 읽어
//! bounded 채널로 전달한다. 재생 속도는 소비자(렌더 루프) 쪽에서
//! fps 기반으로 페이싱하고, 파이프/채널 backpressure가 디코드 속도를
//! 자연스럽게 제한한다. C 바인딩(libclang, FFmpeg dev lib) 없이 빌드되므로
//! 어느 PC에서든 `cargo build`만으로 빌드할 수 있고,
//! 배포 시에는 `ffmpeg.exe`(+DLL)만 exe 옆에 두면 된다.
//!
//! preroll 정책 (OnePlayer 0.4.0 계승):
//! - T-8초에 muted 상태로 디코드를 시작한다 (`-an`, 오디오 없음)
//! - 첫 프레임 준비 신호(`has_first_frame`) 후에만 switch를 허용한다
//! - 첫 프레임 이후 디코드는 파이프 backpressure로 정지 상태를 유지하다가
//!   표출 시작(`decode_next_frame` 첫 호출) 시점부터 진행된다
//! - 디코더는 pool(2개)에서 재사용해 생성/해제 비용과 OOM을 줄인다

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// 디코드된 영상 프레임 (RGBA, GPU 업로드용).
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 영상 디코더 추상화.
///
/// 사용 순서: `open` → `preroll` → (`has_first_frame` 확인)
/// → `decode_next_frame` 반복 → `stop` → 다음 scene에서 `open`으로 재사용.
pub trait VideoDecoder: Send {
    /// 로컬 영상 파일을 연다. 출력 프레임은 `target_width x target_height`
    /// RGBA로 스케일링된다. 첫 프레임 상태는 초기화된다.
    fn open(
        &mut self,
        path: &Path,
        target_width: u32,
        target_height: u32,
        loop_playback: bool,
    ) -> Result<()>;
    /// muted 디코드를 시작한다 (T-8초 시점 호출).
    fn preroll(&mut self) -> Result<()>;
    /// 표출할 프레임이 있으면 반환한다. 표출 시각 전이거나 준비 전이면 `None`.
    /// 첫 호출이 재생 시작 시점으로 기록된다.
    fn decode_next_frame(&mut self) -> Result<Option<VideoFrame>>;
    /// 재생 위치를 처음으로 되돌린다 (loop 재생용).
    fn seek_to_start(&mut self) -> Result<()>;
    /// 첫 프레임이 표출 가능한 상태인지 확인한다 (switch 게이트).
    fn has_first_frame(&self) -> bool;
    /// 첫 프레임 준비 완료 시각 (진단 지표 `preparedFirstFrame`).
    fn first_frame_at_millis(&self) -> Option<i64>;
    /// 첫 프레임 상태를 초기화한다 (scene 교체 시).
    fn reset_first_frame(&mut self);
    /// 디코드를 완전히 중지하고 리소스를 해제한다 (scene 종료 시).
    fn stop(&mut self);
}

/// ffmpeg 실행 파일을 찾지 못했을 때 사용하는 스텁 디코더.
/// preroll 즉시 첫 프레임 준비 완료로 처리한다 (전환 흐름 테스트용).
pub struct StubVideoDecoder {
    first_frame_ready: bool,
    first_frame_at: Option<i64>,
}

impl StubVideoDecoder {
    /// 초기 상태(첫 프레임 미준비)로 생성한다.
    pub fn new() -> Self {
        Self {
            first_frame_ready: false,
            first_frame_at: None,
        }
    }
}

impl Default for StubVideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder for StubVideoDecoder {
    fn open(&mut self, _path: &Path, _w: u32, _h: u32, _loop_playback: bool) -> Result<()> {
        Ok(())
    }

    /// 즉시 첫 프레임 준비 완료로 표시한다 (실제 디코드 없음).
    fn preroll(&mut self) -> Result<()> {
        self.first_frame_ready = true;
        self.first_frame_at = Some(chrono::Utc::now().timestamp_millis());
        Ok(())
    }

    /// 준비 완료 후에는 2x2 검정 프레임을 반환한다.
    fn decode_next_frame(&mut self) -> Result<Option<VideoFrame>> {
        if self.first_frame_ready {
            Ok(Some(VideoFrame {
                width: 2,
                height: 2,
                rgba: vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
            }))
        } else {
            Ok(None)
        }
    }

    fn seek_to_start(&mut self) -> Result<()> {
        Ok(())
    }

    fn has_first_frame(&self) -> bool {
        self.first_frame_ready
    }

    fn first_frame_at_millis(&self) -> Option<i64> {
        self.first_frame_at
    }

    fn reset_first_frame(&mut self) {
        self.first_frame_ready = false;
        self.first_frame_at = None;
    }

    fn stop(&mut self) {
        self.reset_first_frame();
    }
}

/// 진행 중인 ffmpeg 디코드 세션 (프로세스 + 리더 스레드 + 채널).
struct DecodeSession {
    child: Child,
    reader: Option<JoinHandle<()>>,
    /// 리더 스레드 종료 신호.
    stop_flag: Arc<AtomicBool>,
    /// 표출 시작 신호. true가 되면 리더가 첫 프레임 이후를 계속 읽는다.
    start_flag: Arc<AtomicBool>,
    /// preroll로 준비된 첫 프레임 (표출 시작 시 1회 소비).
    first_frame: Arc<Mutex<Option<VideoFrame>>>,
    /// 첫 프레임 준비 완료 플래그 (소비 후에도 유지).
    first_frame_ready: Arc<AtomicBool>,
    /// 첫 프레임 준비 완료 시각 (epoch millis, 0 = 미준비).
    first_frame_at: Arc<AtomicI64>,
    /// 디코드된 프레임 채널 (bounded — 디코드 선행량 제한).
    frames: Receiver<VideoFrame>,
}

/// FFmpeg CLI(`ffmpeg.exe`) 기반 영상 디코더.
///
/// DID 정책상 영상은 무음(H.264, B-frame 없음, CFR)이므로
/// 오디오 동기화 없이 프레임 디코드 파이프라인만 구성한다.
pub struct FfmpegCliDecoder {
    ffmpeg_exe: PathBuf,
    ffprobe_exe: Option<PathBuf>,
    /// `none`/빈 문자열이면 CPU 디코딩.
    hwaccel: String,
    path: Option<PathBuf>,
    target_width: u32,
    target_height: u32,
    loop_playback: bool,
    /// 영상 fps (ffprobe로 조회, 실패 시 30).
    fps: f64,
    session: Option<DecodeSession>,
    /// 표출 시작 시각 (decode_next_frame 첫 호출 시점, 페이싱 기준).
    playback_started: Option<Instant>,
    /// 지금까지 표출(소비)한 프레임 수.
    frames_consumed: u64,
}

impl FfmpegCliDecoder {
    /// ffmpeg 실행 파일을 찾아 디코더를 만든다. 없으면 에러.
    pub fn new() -> Result<Self> {
        Self::with_hwaccel(default_hwaccel())
    }

    /// hwaccel 방식을 지정해 디코더를 만든다.
    pub fn with_hwaccel(hwaccel: impl Into<String>) -> Result<Self> {
        let ffmpeg_exe = find_ffmpeg_tool(ffmpeg_binary_name())
            .context("ffmpeg executable not found (checked ONEPLAYER_FFMPEG_DIR, exe dir, tools/ffmpeg, PATH)")?;
        let ffprobe_exe = find_ffmpeg_tool(ffprobe_binary_name());
        let hwaccel = normalize_hwaccel(hwaccel.into());
        if hwaccel_enabled(&hwaccel) {
            info!(path = %ffmpeg_exe.display(), hwaccel = %hwaccel, "ffmpeg found");
        } else {
            info!(path = %ffmpeg_exe.display(), "ffmpeg found (software decode)");
        }
        Ok(Self {
            ffmpeg_exe,
            ffprobe_exe,
            hwaccel,
            path: None,
            target_width: 0,
            target_height: 0,
            loop_playback: false,
            fps: 30.0,
            session: None,
            playback_started: None,
            frames_consumed: 0,
        })
    }

    /// ffmpeg 프로세스를 스폰하고 리더 스레드를 시작한다.
    fn spawn_session(&mut self) -> Result<DecodeSession> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("video path not set"))?;
        let (w, h) = (self.target_width.max(2), self.target_height.max(2));

        let mut cmd = Command::new(&self.ffmpeg_exe);
        cmd.args(["-hide_banner", "-loglevel", "error"]);
        append_hwaccel_args(&mut cmd, &self.hwaccel);
        if self.loop_playback {
            // 입력을 무한 반복한다. scene 종료 시 stop()으로 끊는다.
            cmd.args(["-stream_loop", "-1"]);
        }
        cmd.arg("-i")
            .arg(path)
            .args(["-an", "-sn"])
            .arg("-vf")
            .arg(video_filter(&self.hwaccel, w, h))
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_hidden_window(&mut cmd);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn ffmpeg for {}", path.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg stdout unavailable"))?;
        let stderr = child.stderr.take();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let start_flag = Arc::new(AtomicBool::new(false));
        let first_frame = Arc::new(Mutex::new(None));
        let first_frame_ready = Arc::new(AtomicBool::new(false));
        let first_frame_at = Arc::new(AtomicI64::new(0));
        // 디코드 선행량 제한: 채널 3프레임 + OS 파이프 버퍼.
        let (tx, rx) = std::sync::mpsc::sync_channel::<VideoFrame>(3);

        let reader = std::thread::spawn({
            let stop_flag = stop_flag.clone();
            let start_flag = start_flag.clone();
            let first_frame = first_frame.clone();
            let first_frame_ready = first_frame_ready.clone();
            let first_frame_at = first_frame_at.clone();
            move || {
                reader_loop(
                    stdout,
                    w,
                    h,
                    first_frame,
                    first_frame_ready,
                    first_frame_at,
                    start_flag,
                    stop_flag,
                    tx,
                );
            }
        });
        if let Some(stderr) = stderr {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let mut pipe = stderr;
                let _ = pipe.read_to_string(&mut buf);
                if !buf.trim().is_empty() {
                    warn!(stderr = %buf.trim(), "ffmpeg stderr");
                }
            });
        }

        debug!(path = %path.display(), w, h, fps = self.fps, hwaccel = %self.hwaccel, "ffmpeg decode session started");
        Ok(DecodeSession {
            child,
            reader: Some(reader),
            stop_flag,
            start_flag,
            first_frame,
            first_frame_ready,
            first_frame_at,
            frames: rx,
        })
    }

    /// hwaccel 실패 시 소프트웨어 디코딩으로 한 번 재시도한다.
    fn spawn_with_fallback(&mut self) -> Result<()> {
        let requested = self.hwaccel.clone();
        match self.wait_for_first_frame() {
            Ok(()) => Ok(()),
            Err(err) if hwaccel_enabled(&requested) => {
                warn!(
                    hwaccel = %requested,
                    error = %err,
                    "hardware decode failed, falling back to software"
                );
                self.hwaccel = String::new();
                self.wait_for_first_frame()
            }
            Err(err) => Err(err),
        }
    }

    /// ffmpeg 세션을 시작하고 첫 프레임 준비를 기다린다.
    fn wait_for_first_frame(&mut self) -> Result<()> {
        self.stop_session();
        let session = self.spawn_session()?;
        self.session = Some(session);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let session = self.session.as_mut().expect("session");
            if session.first_frame_ready.load(Ordering::SeqCst) {
                return Ok(());
            }
            if let Ok(Some(status)) = session.child.try_wait() {
                self.session.take();
                anyhow::bail!("ffmpeg exited before first frame ({status})");
            }
            if Instant::now() >= deadline {
                self.stop_session();
                anyhow::bail!("ffmpeg first frame timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 진행 중인 세션을 종료한다 (프로세스 kill + 리더 join).
    fn stop_session(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stop_flag.store(true, Ordering::SeqCst);
            let _ = session.child.kill();
            let _ = session.child.wait();
            if let Some(handle) = session.reader.take() {
                let _ = handle.join();
            }
        }
        self.playback_started = None;
        self.frames_consumed = 0;
    }
}

impl Drop for FfmpegCliDecoder {
    fn drop(&mut self) {
        self.stop_session();
    }
}

impl VideoDecoder for FfmpegCliDecoder {
    /// 파일 경로와 출력 크기를 기억하고, 이전 세션을 정리한다.
    fn open(
        &mut self,
        path: &Path,
        target_width: u32,
        target_height: u32,
        loop_playback: bool,
    ) -> Result<()> {
        // 파일 없음(ENOENT)은 hwaccel과 무관한 실패이므로 여기서 조기에 걸러낸다.
        // (ffmpeg가 exit -2로 죽으면 하드웨어 디코드 실패로 오인돼 원인이 가려진다.)
        anyhow::ensure!(
            path.is_file(),
            "video file not found: {}",
            path.display()
        );
        self.stop_session();
        self.path = Some(path.to_path_buf());
        self.target_width = target_width;
        self.target_height = target_height;
        self.loop_playback = loop_playback;
        self.fps = self
            .ffprobe_exe
            .as_deref()
            .and_then(|probe| probe_fps(probe, path))
            .filter(|f| (1.0..=240.0).contains(f))
            .unwrap_or(30.0);
        Ok(())
    }

    /// muted 디코드를 시작한다 (T-8초 시점 호출).
    fn preroll(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(()); // 이미 preroll됨.
        }
        self.spawn_with_fallback()
    }

    /// fps 페이싱에 맞춰 표출할 프레임을 반환한다.
    ///
    /// - 첫 호출: 재생 시작으로 기록하고 preroll된 첫 프레임을 반환
    /// - 이후: 표출 시각이 된 프레임만 채널에서 꺼낸다.
    ///   렌더 루프가 밀렸으면 due까지 여러 프레임을 꺼내 최신 것만 반환(드롭)
    fn decode_next_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some(session) = &self.session else {
            return Ok(None);
        };

        // 첫 호출: 표출 시작. preroll된 첫 프레임을 반환한다.
        let Some(started) = self.playback_started else {
            if !session.first_frame_ready.load(Ordering::SeqCst) {
                return Ok(None);
            }
            session.start_flag.store(true, Ordering::SeqCst);
            self.playback_started = Some(Instant::now());
            self.frames_consumed = 1;
            let frame = session.first_frame.lock().expect("first frame lock").take();
            return Ok(frame);
        };

        // 페이싱: 지금까지 표출됐어야 할 프레임 수 = floor(elapsed*fps)+1.
        let elapsed = started.elapsed().as_secs_f64();
        let due = (elapsed * self.fps) as u64 + 1;
        let mut latest = None;
        while self.frames_consumed < due {
            match session.frames.try_recv() {
                Ok(frame) => {
                    latest = Some(frame);
                    self.frames_consumed += 1;
                }
                // 디코더가 아직 못 따라왔거나(EOF 포함) 채널이 비어 있음.
                Err(_) => break,
            }
        }
        Ok(latest)
    }

    /// 처음부터 다시 재생한다 (세션 재시작).
    fn seek_to_start(&mut self) -> Result<()> {
        if self.session.is_some() {
            self.stop_session();
            self.preroll()?;
        }
        Ok(())
    }

    fn has_first_frame(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.first_frame_ready.load(Ordering::SeqCst))
    }

    fn first_frame_at_millis(&self) -> Option<i64> {
        let at = self
            .session
            .as_ref()
            .map(|s| s.first_frame_at.load(Ordering::SeqCst))
            .unwrap_or(0);
        (at != 0).then_some(at)
    }

    fn reset_first_frame(&mut self) {
        self.stop_session();
    }

    fn stop(&mut self) {
        self.stop_session();
    }
}

/// 리더 스레드 본체: 파이프에서 프레임을 읽어 채널로 보낸다.
///
/// 1. 첫 프레임을 읽어 first_frame 슬롯에 저장 (preroll 완료 신호)
/// 2. 표출 시작(start_flag)까지 대기 — 이 동안 파이프 backpressure로
///    ffmpeg 디코드가 정지 상태를 유지한다
/// 3. 이후 프레임을 계속 읽어 bounded 채널로 전달 (가득 차면 대기)
#[allow(clippy::too_many_arguments)]
fn reader_loop(
    mut stdout: impl Read,
    width: u32,
    height: u32,
    first_frame: Arc<Mutex<Option<VideoFrame>>>,
    first_frame_ready: Arc<AtomicBool>,
    first_frame_at: Arc<AtomicI64>,
    start_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    tx: SyncSender<VideoFrame>,
) {
    let frame_bytes = (width as usize) * (height as usize) * 4;
    let mut buf = vec![0u8; frame_bytes];

    // 1. 첫 프레임 (preroll 게이트).
    if stdout.read_exact(&mut buf).is_err() {
        warn!("ffmpeg produced no first frame");
        return;
    }
    *first_frame.lock().expect("first frame lock") = Some(VideoFrame {
        width,
        height,
        rgba: buf.clone(),
    });
    first_frame_at.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
    first_frame_ready.store(true, Ordering::SeqCst);

    // 2. 표출 시작 대기.
    while !start_flag.load(Ordering::SeqCst) {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // 3. 연속 디코드. EOF(비loop) 또는 stop 시 종료.
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        if stdout.read_exact(&mut buf).is_err() {
            return; // EOF 또는 프로세스 종료.
        }
        let mut frame = VideoFrame {
            width,
            height,
            rgba: buf.clone(),
        };
        // 채널이 가득 차면 소비될 때까지 대기 (디코드 선행량 제한).
        loop {
            match tx.try_send(frame) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    if stop_flag.load(Ordering::SeqCst) {
                        return;
                    }
                    frame = returned;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

/// ffprobe로 영상의 평균 fps를 조회한다 (`avg_frame_rate`, 예: "30000/1001").
fn probe_fps(ffprobe: &Path, video: &Path) -> Option<f64> {
    let mut cmd = Command::new(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=avg_frame_rate",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ])
    .arg(video)
    .stdin(Stdio::null())
    .stderr(Stdio::null());
    configure_hidden_window(&mut cmd);

    let output = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim().to_string();
    let fps = match line.split_once('/') {
        Some((num, den)) => {
            let num: f64 = num.trim().parse().ok()?;
            let den: f64 = den.trim().parse().ok()?;
            if den == 0.0 {
                return None;
            }
            num / den
        }
        None => line.parse().ok()?,
    };
    debug!(video = %video.display(), fps, "probed video fps");
    Some(fps)
}

/// Windows에서 자식 프로세스 콘솔 창이 뜨지 않게 한다.
#[cfg(windows)]
fn configure_hidden_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_window(_cmd: &mut Command) {}

/// 플랫폼별 ffmpeg 실행 파일 이름.
fn ffmpeg_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// 플랫폼별 ffprobe 실행 파일 이름.
fn ffprobe_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }
}

/// ffmpeg/ffprobe 실행 파일을 탐색한다.
///
/// 우선순위: `ONEPLAYER_FFMPEG_DIR` 환경변수 → exe와 같은 폴더
/// → 작업 디렉터리의 `tools/ffmpeg` → PATH.
fn find_ffmpeg_tool(name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ONEPLAYER_FFMPEG_DIR") {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let candidate = PathBuf::from("tools").join("ffmpeg").join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 사용 가능한 디코더 구현을 생성한다.
/// ffmpeg 실행 파일이 있으면 [`FfmpegCliDecoder`], 없으면 스텁(검정 프레임).
pub fn create_decoder(hwaccel: &str) -> Box<dyn VideoDecoder> {
    match FfmpegCliDecoder::with_hwaccel(hwaccel) {
        Ok(decoder) => Box::new(decoder),
        Err(err) => {
            warn!("ffmpeg unavailable, falling back to stub video decoder: {err:#}");
            Box::new(StubVideoDecoder::new())
        }
    }
}

/// 영상 디코더 pool.
///
/// scene마다 디코더를 생성/해제하지 않고 제한된 slot을 재사용한다
/// (Android VideoPlayerPool 정책 — decoder churn과 OOM 방지).
pub struct VideoDecoderPool {
    decoders: Vec<Arc<Mutex<Box<dyn VideoDecoder>>>>,
    /// 다음에 빌려줄 slot 인덱스 (라운드 로빈).
    next_index: std::sync::atomic::AtomicUsize,
}

impl VideoDecoderPool {
    /// 지정 개수의 디코더 slot을 만든다 (최소 1).
    pub fn new(size: usize, hwaccel: impl Into<String>) -> Self {
        let hwaccel = normalize_hwaccel(hwaccel.into());
        let decoders = (0..size.max(1))
            .map(|_| Arc::new(Mutex::new(create_decoder(&hwaccel))))
            .collect();
        Self {
            decoders,
            next_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 라운드 로빈으로 디코더 slot을 빌려준다.
    pub fn acquire(&self) -> Arc<Mutex<Box<dyn VideoDecoder>>> {
        let idx = self
            .next_index
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            % self.decoders.len();
        self.decoders[idx].clone()
    }
}

fn default_hwaccel() -> String {
    if cfg!(windows) {
        "d3d11va".into()
    } else {
        String::new()
    }
}

fn normalize_hwaccel(hwaccel: String) -> String {
    hwaccel.trim().to_ascii_lowercase()
}

fn hwaccel_enabled(hwaccel: &str) -> bool {
    !hwaccel.is_empty() && hwaccel != "none" && hwaccel != "off" && hwaccel != "software"
}

/// `-i` 이전에 붙는 hwaccel 인자.
fn append_hwaccel_args(cmd: &mut Command, hwaccel: &str) {
    if !hwaccel_enabled(hwaccel) {
        return;
    }
    match hwaccel {
        "cuda" => {
            cmd.args(["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
        }
        "qsv" => {
            cmd.args(["-hwaccel", "qsv"]);
        }
        other => {
            cmd.args(["-hwaccel", other]);
        }
    }
}

/// hwaccel 방식에 맞는 `-vf` 필터 문자열.
///
/// d3d11va/dxva2 등은 `scale`만 쓰면 FFmpeg가 CPU로 자동 전환한다.
/// `hwdownload`를 앞에 붙이면 필터 체인 오류로 디코드가 실패할 수 있다.
fn video_filter(hwaccel: &str, width: u32, height: u32) -> String {
    let (w, h) = (width.max(2), height.max(2));
    match hwaccel {
        "cuda" => format!("scale_cuda={w}:{h},hwdownload,format=rgba"),
        "qsv" => format!("scale_qsv=w={w}:h={h},hwdownload,format=rgba"),
        "opencl" => format!("scale_opencl={w}:{h},hwdownload,format=rgba"),
        _ if hwaccel_enabled(hwaccel) => format!("scale={w}:{h}"),
        _ => format!("scale={w}:{h}"),
    }
}

#[cfg(test)]
mod hwaccel_tests {
    use super::{append_hwaccel_args, hwaccel_enabled, normalize_hwaccel, video_filter};
    use std::process::Command;

    #[test]
    fn normalize_hwaccel_values() {
        assert_eq!(normalize_hwaccel(" D3D11VA ".into()), "d3d11va");
        assert!(!hwaccel_enabled("none"));
        assert!(hwaccel_enabled("cuda"));
    }

    #[test]
    fn cuda_filter_uses_scale_cuda() {
        assert_eq!(
            video_filter("cuda", 1920, 1080),
            "scale_cuda=1920:1080,hwdownload,format=rgba"
        );
    }

    #[test]
    fn d3d11va_filter_uses_cpu_scale() {
        assert_eq!(video_filter("d3d11va", 1080, 1920), "scale=1080:1920");
    }

    #[test]
    fn append_cuda_hwaccel_args() {
        let mut cmd = Command::new("ffmpeg");
        append_hwaccel_args(&mut cmd, "cuda");
        let args: Vec<_> = cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["-hwaccel", "cuda"]));
    }
}
