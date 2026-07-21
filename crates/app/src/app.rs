//! 애플리케이션 상태: 엔진(core)과 렌더러(render)를 조립한다.
//!
//! 데이터 흐름:
//! 1. 엔진(tokio 백그라운드)이 전환 명령([`SwitchCommand`])을 채널로 보낸다
//! 2. 렌더 루프(winit `RedrawRequested`)가 명령을 받아 scene prepare(백그라운드)
//!    → hidden 레이어 preload → 전환 예약
//! 3. 매 프레임 [`DoubleBufferCompositor::tick`]이 목표 시각 도달을 검사해 전환
//! 4. 전환 결과(delay)를 엔진 콜백으로 회신해 진단 로그에 남긴다

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use oneplayer_core::clock::{Clock, SignageClock};
use oneplayer_core::engine::{EngineEvent, PlaybackEngine, SwitchCommand};
use oneplayer_core::settings::AppSettings;
use oneplayer_core::timing_log::{self, TimingValue};
use oneplayer_render::{DoubleBufferCompositor, ScenePreparer};
use tokio::sync::mpsc;
use tracing::{error, info};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;

use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use crate::sample;
use crate::settings_ui::{SettingsAction, SettingsUi};

/// 백그라운드 prepare 완료 결과.
enum PrepareOutcome {
    Ready {
        scene: oneplayer_render::PreparedScene,
        target_time_millis: i64,
    },
    Failed {
        scene_id: String,
        target_time_millis: i64,
        reason: String,
    },
}

/// 앱 전체 상태. winit `ApplicationHandler`로 이벤트 루프에 연결된다.
pub struct App {
    /// 앱 설정.
    settings: AppSettings,
    /// 설정 파일 경로 (저장 시 사용).
    config_path: PathBuf,
    /// 보정 클럭 (엔진과 컴포지터가 공유).
    clock: Arc<SignageClock>,
    /// 재생 엔진 (tokio 백그라운드에서 동작).
    engine: Option<Arc<PlaybackEngine>>,
    /// 더블버퍼 컴포지터 (창 생성 후 초기화).
    compositor: Option<DoubleBufferCompositor>,
    /// scene 준비 담당 (창 생성 후 초기화).
    preparer: Option<Arc<Mutex<ScenePreparer>>>,
    /// 백그라운드 prepare 완료 수신.
    prepared_rx: std_mpsc::Receiver<PrepareOutcome>,
    /// 백그라운드 prepare 완료 발신 (spawn_blocking에서 사용).
    prepared_tx: std_mpsc::Sender<PrepareOutcome>,
    /// prepare 진행 중인 scene_id (중복 스폰 방지).
    preparing: HashSet<String>,
    /// 메인 창 핸들.
    window: Option<Arc<Window>>,
    /// 엔진용 tokio 런타임 (winit이 메인 스레드를 점유하므로 별도 운용).
    /// drop 시 백그라운드 태스크가 함께 종료되므로 보관만 한다.
    #[allow(dead_code)]
    runtime: tokio::runtime::Runtime,
    /// 엔진 → 렌더 루프 전환 명령 수신 채널.
    switch_rx: mpsc::UnboundedReceiver<SwitchCommand>,
    /// `--sample` 데모 모드 여부.
    sample_mode: bool,
    /// 데모 모드의 scene B 예약 상태.
    sample_schedule: Option<sample::SampleSchedule>,
    /// 설정 UI 오버레이 (창 생성 후 초기화).
    settings_ui: Option<SettingsUi>,
}

impl App {
    /// 엔진과 채널을 초기화하고 백그라운드 작업을 시작한다.
    /// 렌더 관련 리소스(창/컴포지터)는 winit `resumed`에서 만든다.
    pub fn new(settings: AppSettings, config_path: PathBuf, sample_mode: bool) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let clock = Arc::new(SignageClock::new(&settings));
        let (switch_tx, switch_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (prepared_tx, prepared_rx) = std_mpsc::channel();

        // 엔진 생성 후 동기화/재생 루프를 백그라운드로 시작한다.
        let engine = Arc::new(PlaybackEngine::new(
            settings.clone(),
            clock.clone(),
            event_tx,
            switch_tx,
        )?);
        runtime.spawn({
            let engine = engine.clone();
            async move { engine.start().await }
        });

        // 엔진 이벤트를 로그로 흘린다 (debug overlay의 데이터 소스).
        runtime.spawn(log_engine_events(event_rx));

        Ok(Self {
            settings,
            config_path,
            clock,
            engine: Some(engine),
            compositor: None,
            preparer: None,
            prepared_rx,
            prepared_tx,
            preparing: HashSet::new(),
            window: None,
            runtime,
            switch_rx,
            sample_mode,
            sample_schedule: None,
            settings_ui: None,
        })
    }

    /// 설정 UI 커서 표시 여부를 갱신한다.
    fn update_cursor_visibility(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let show = self
            .settings_ui
            .as_ref()
            .map(|ui| ui.wants_pointer(&self.settings) || ui.is_panel_open())
            .unwrap_or(false);
        window.set_cursor_visible(show || !self.settings.fullscreen);
    }

    /// 설정 저장 후 런타임에 반영 가능한 항목을 적용한다.
    fn apply_saved_settings(&mut self, updated: AppSettings) {
        let canvas_changed = updated.canvas_width != self.settings.canvas_width
            || updated.canvas_height != self.settings.canvas_height;
        let fullscreen_changed = updated.fullscreen != self.settings.fullscreen;

        self.settings = updated;
        if let Some(ui) = self.settings_ui.as_mut() {
            ui.sync_from_settings(&self.settings);
        }

        if fullscreen_changed {
            if let Some(window) = &self.window {
                if self.settings.fullscreen {
                    window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
                } else {
                    window.set_fullscreen(None);
                }
            }
        }

        if canvas_changed {
            if let Some(window) = &self.window {
                let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                    self.settings.canvas_width,
                    self.settings.canvas_height,
                ));
            }
            if let Some(compositor) = self.compositor.as_mut() {
                compositor.resize(self.settings.canvas_width, self.settings.canvas_height);
            }
        }

        self.update_cursor_visibility();
    }

    /// 엔진이 보낸 전환 명령들을 처리한다:
    /// scene prepare(백그라운드) → hidden 레이어 preload → 목표 시각 전환 예약.
    /// prepare 실패 시 엔진에 실패를 회신한다 (현재 화면 유지 fallback).
    fn handle_switch_commands(&mut self) {
        // 1. 이전 프레임에서 spawn_blocking으로 시작한 prepare 결과를 먼저 회수한다.
        //    try_recv는 대기하지 않으므로, 결과가 없으면 즉시 다음 단계로 넘어간다.
        while let Ok(outcome) = self.prepared_rx.try_recv() {
            match outcome {
                PrepareOutcome::Ready {
                    scene,
                    target_time_millis,
                } => {
                    // prepare가 끝났으므로 중복 prepare 방지 목록에서 제거한다.
                    self.preparing.remove(&scene.scene.scene_id);
                    if let Some(compositor) = self.compositor.as_mut() {
                        let scene_id = scene.scene.scene_id.clone();
                        let preload_started = Instant::now();
                        let preload_now = self.clock.now_millis();
                        // 준비된 scene을 hidden 레이어에 GPU 리소스로 올린다.
                        // 실제 표출 시점에는 이 무거운 업로드를 하지 않는다.
                        compositor.preload(scene);
                        let gpu_preload_ms = preload_started.elapsed().as_millis();
                        let scheduled_now = self.clock.now_millis();
                        // 목표 시각이 되면 hidden 레이어가 active가 되도록 예약한다.
                        compositor.switch_at(target_time_millis);
                        timing_log::record(
                            "INFO",
                            11,
                            "GPU_PRELOAD_DONE",
                            Some(&scene_id),
                            Some(target_time_millis),
                            Some(preload_now),
                            vec![("duration_ms", TimingValue::from(gpu_preload_ms))],
                        );
                        timing_log::record(
                            "INFO",
                            12,
                            "SWITCH_AT_SCHEDULED",
                            Some(&scene_id),
                            Some(target_time_millis),
                            Some(scheduled_now),
                            vec![],
                        );
                    }
                }
                PrepareOutcome::Failed {
                    scene_id,
                    target_time_millis,
                    reason,
                } => {
                    // prepare 실패 시 해당 scene은 전환하지 않고 현재 화면을 유지한다.
                    self.preparing.remove(&scene_id);
                    timing_log::record(
                        "ERROR",
                        "ERROR",
                        "PREPARE_FAILED",
                        Some(&scene_id),
                        Some(target_time_millis),
                        Some(self.clock.now_millis()),
                        vec![("exception", TimingValue::from(reason.clone()))],
                    );
                    if let Some(engine) = &self.engine {
                        engine.on_switch_failed(&scene_id, &reason);
                    }
                }
            }
        }

        // 2. 엔진(core)이 새로 보낸 전환 명령을 모두 꺼내 prepare 작업을 시작한다.
        //    명령 수신 자체는 렌더 스레드에서 짧게 처리하고, 무거운 준비는 아래에서 분리한다.
        while let Ok(cmd) = self.switch_rx.try_recv() {
            let Some(preparer) = self.preparer.clone() else {
                return;
            };
            // 같은 scene prepare가 이미 진행 중이면 새 작업을 만들지 않는다.
            if !self.preparing.insert(cmd.scene.scene_id.clone()) {
                continue;
            }
            // blocking 스레드로 넘길 값들은 move closure가 소유할 수 있게 복사한다.
            let tx = self.prepared_tx.clone();
            let scene = cmd.scene.clone();
            let local_files = cmd.local_files.clone();
            let target_time_millis = cmd.target_time_millis;
            let now_millis = self.clock.now_millis();
            let scene_id = scene.scene_id.clone();
            timing_log::record(
                "INFO",
                4,
                "SWITCH_COMMAND_RECEIVED",
                Some(&scene_id),
                Some(target_time_millis),
                Some(now_millis),
                vec![
                    ("asset_count", TimingValue::from(scene.asset_refs.len())),
                    ("is_video", TimingValue::from(scene.has_video())),
                ],
            );
            let clock = self.clock.clone();
            // 이미지 decode, 영상 open/preroll은 오래 걸릴 수 있으므로 렌더 루프에서 직접 하지 않는다.
            // 별도 blocking 스레드에서 처리해 기존 화면 렌더링이 계속 돌 수 있게 한다.
            self.runtime.spawn_blocking(move || {
                let prepare_started = Instant::now();
                let prepare_start_now = clock.now_millis();
                timing_log::record(
                    "INFO",
                    5,
                    "PREPARE_BLOCKING_STARTED",
                    Some(&scene_id),
                    Some(target_time_millis),
                    Some(prepare_start_now),
                    vec![],
                );
                let outcome = match preparer.lock() {
                    // ScenePreparer는 이미지 캐시와 디코더 pool을 소유하므로 한 번에 하나씩 접근한다.
                    Ok(mut preparer) => match preparer.prepare(&scene, &local_files, now_millis) {
                        Ok(prepared) => {
                            timing_log::record(
                                "INFO",
                                10,
                                "PREPARE_DONE",
                                Some(&scene_id),
                                Some(target_time_millis),
                                Some(clock.now_millis()),
                                vec![(
                                    "duration_ms",
                                    TimingValue::from(prepare_started.elapsed().as_millis()),
                                )],
                            );
                            PrepareOutcome::Ready {
                                // 성공 결과는 메인 렌더 루프가 다음 프레임에서 GPU preload한다.
                                scene: prepared,
                                target_time_millis,
                            }
                        }
                        Err(err) => {
                            timing_log::record(
                                "ERROR",
                                "ERROR",
                                "PREPARE_EXCEPTION",
                                Some(&scene_id),
                                Some(target_time_millis),
                                Some(clock.now_millis()),
                                vec![
                                    ("exception", TimingValue::from(err.to_string())),
                                    (
                                        "duration_ms",
                                        TimingValue::from(prepare_started.elapsed().as_millis()),
                                    ),
                                ],
                            );
                            PrepareOutcome::Failed {
                                scene_id: scene_id.clone(),
                                target_time_millis,
                                reason: err.to_string(),
                            }
                        }
                    },
                    // mutex가 poison되면 prepare 자체를 실패로 보고 엔진에 알린다.
                    Err(err) => PrepareOutcome::Failed {
                        scene_id,
                        target_time_millis,
                        reason: err.to_string(),
                    },
                };
                // blocking 작업 결과를 렌더 루프 쪽으로 돌려보낸다.
                let _ = tx.send(outcome);
            });
        }
    }

    /// 매 프레임 수행: 전환 시각 검사(tick) → 결과 회신 → 화면 그리기.
    fn render_frame(&mut self) {
        // 설정 UI가 저장을 요청하면, 렌더 중 borrow 충돌을 피하려고 일단 값만 빼둔다.
        let mut pending_save: Option<AppSettings> = None;
        if let (Some(ui), Some(window)) = (self.settings_ui.as_mut(), self.window.as_ref()) {
            if let SettingsAction::Save(updated) = ui.update(window, &self.settings) {
                pending_save = Some(updated);
            }
            self.update_cursor_visibility();
        }
        // UI borrow가 끝난 뒤 실제 파일 저장과 런타임 반영을 수행한다.
        if let Some(updated) = pending_save {
            match updated.save(&self.config_path) {
                Ok(()) => {
                    info!(path = %self.config_path.display(), "settings saved");
                    self.apply_saved_settings(updated);
                }
                Err(err) => error!(%err, "settings save failed"),
            }
        }

        // 데모 모드: scene B의 prepare 시각이 되면 예약한다.
        if self.sample_mode {
            if let (Some(schedule), Some(preparer), Some(compositor)) = (
                self.sample_schedule.as_mut(),
                self.preparer.as_ref(),
                self.compositor.as_mut(),
            ) {
                sample::maybe_prepare_sample_b(
                    self.clock.now_millis(),
                    schedule,
                    preparer,
                    compositor,
                );
            }
        }

        // overlay 렌더링에는 SettingsUi와 Window가 필요하지만,
        // 아래에서 compositor를 mutable로 빌리므로 필요한 핸들만 미리 분리한다.
        let overlay = self
            .settings_ui
            .as_mut()
            .zip(self.window.clone())
            .map(|(ui, window)| (ui as *mut SettingsUi, window));

        let Some(compositor) = self.compositor.as_mut() else {
            return;
        };
        // 정밀 전환 검사: 목표 시각에 도달한 첫 프레임에서 전환된다.
        if let Some(result) = compositor.tick() {
            // 전환 완료 시간을 엔진에 알려 재생 로그와 복구 판단 기준을 갱신한다.
            if let Some(engine) = &self.engine {
                engine.on_scene_switched(
                    &result.scene_id,
                    result.target_time_millis,
                    result.actual_time_millis,
                );
            }
        }

        if let Some((ui_ptr, window)) = overlay {
            // scene을 먼저 그리고, 같은 render pass 흐름 안에서 설정 UI를 덧그린다.
            let (device, queue, _) = compositor.gpu_resources();
            let device = device.clone();
            let queue = queue.clone();
            let _ = compositor.render_with_overlay(|encoder, view, width, height| {
                // SAFETY: ui는 render_frame 동안 App이 소유하며, overlay는 동기 호출된다.
                let ui = unsafe { &mut *ui_ptr };
                ui.render(&device, &queue, encoder, view, width, height, &window);
            });
        } else {
            // overlay가 없으면 scene만 렌더링한다.
            let _ = compositor.render();
        }
    }
}

impl ApplicationHandler for App {
    /// 창 생성 시점 (winit 요구사항: 창은 resumed에서 만든다).
    /// 전체화면 설정, 절전 방지, GPU 컴포지터 초기화를 수행한다.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // 이미 초기화됨.
        }

        // 창 생성 (설정된 캔버스 해상도, 필요 시 borderless 전체화면).
        let attrs = WindowAttributes::default()
            .with_title("OnePlayer")
            .with_inner_size(winit::dpi::PhysicalSize::new(
                self.settings.canvas_width,
                self.settings.canvas_height,
            ));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        if self.settings.fullscreen {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            window.set_cursor_visible(false);
        }

        // DID 운영: 화면 꺼짐/절전을 막는다.
        #[cfg(windows)]
        crate::windows_power::prevent_sleep();

        // 컴포지터에 보정 클럭을 주입한다 (시스템 시각 사용 금지).
        let clock = self.clock.clone();
        let now_fn = Box::new(move || clock.now_millis());
        let compositor = pollster::block_on(DoubleBufferCompositor::new(
            window.clone(),
            self.settings.canvas_width,
            self.settings.canvas_height,
            now_fn,
        ))
        .expect("compositor init failed");

        self.preparer = Some(Arc::new(Mutex::new(ScenePreparer::new(
            self.settings.canvas_width,
            self.settings.canvas_height,
            self.settings.ffmpeg_hwaccel.clone(),
        ))));
        self.compositor = Some(compositor);
        self.window = Some(window.clone());

        let (device, _, format) = self.compositor.as_ref().unwrap().gpu_resources();
        let mut settings_ui = SettingsUi::new(&window, device, format);
        settings_ui.sync_from_settings(&self.settings);
        self.settings_ui = Some(settings_ui);
        self.update_cursor_visibility();

        // 데모 모드: scene A를 즉시 예약하고 scene B 스케줄을 만든다.
        if self.sample_mode {
            sample::bootstrap_sample_a(
                &self.clock,
                self.preparer.as_mut().unwrap(),
                self.compositor.as_mut().unwrap(),
            );
            self.sample_schedule = Some(sample::create_sample_schedule(&self.clock));
        }
    }

    /// 창 이벤트 처리: 종료, 리사이즈, 프레임 그리기.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if let (Some(ui), Some(window)) = (self.settings_ui.as_mut(), self.window.as_ref()) {
            let _ = ui.on_window_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(c) = self.compositor.as_mut() {
                    c.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                // 프레임마다: 전환 명령 처리 → tick/렌더 → 다음 프레임 요청.
                self.handle_switch_commands();
                self.render_frame();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// 이벤트가 없어도 계속 프레임을 요청한다
    /// (VSync 주기 렌더 루프 = 정밀 전환 검사 주기).
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// 엔진 이벤트를 tracing 로그로 기록하는 백그라운드 태스크.
async fn log_engine_events(mut event_rx: mpsc::UnboundedReceiver<EngineEvent>) {
    while let Some(event) = event_rx.recv().await {
        match event {
            EngineEvent::SceneSwitched {
                scene_id,
                delay_millis,
                ..
            } => info!(%scene_id, delay_millis, "engine scene switched"),
            EngineEvent::SwitchFailed { scene_id, reason } => {
                error!(%scene_id, %reason, "engine switch failed")
            }
            EngineEvent::Error(msg) => error!(%msg, "engine error"),
            other => info!(?other, "engine event"),
        }
    }
}
