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

use anyhow::Result;
use oneplayer_core::clock::{Clock, SignageClock};
use oneplayer_core::engine::{EngineEvent, PlaybackEngine, SwitchCommand};
use oneplayer_core::settings::AppSettings;
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

        // 엔진 이벤트를 로그로 흘린다 (v2 debug overlay의 데이터 소스).
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

        self.settings = updated;
        if let Some(ui) = self.settings_ui.as_mut() {
            ui.sync_from_settings(&self.settings);
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
        while let Ok(outcome) = self.prepared_rx.try_recv() {
            match outcome {
                PrepareOutcome::Ready {
                    scene,
                    target_time_millis,
                } => {
                    self.preparing.remove(&scene.scene.scene_id);
                    if let Some(compositor) = self.compositor.as_mut() {
                        compositor.preload(scene);
                        compositor.switch_at(target_time_millis);
                    }
                }
                PrepareOutcome::Failed { scene_id, reason } => {
                    self.preparing.remove(&scene_id);
                    if let Some(engine) = &self.engine {
                        engine.on_switch_failed(&scene_id, &reason);
                    }
                }
            }
        }

        while let Ok(cmd) = self.switch_rx.try_recv() {
            let Some(preparer) = self.preparer.clone() else {
                return;
            };
            if !self.preparing.insert(cmd.scene.scene_id.clone()) {
                continue;
            }
            let tx = self.prepared_tx.clone();
            let scene = cmd.scene.clone();
            let local_files = cmd.local_files.clone();
            let target_time_millis = cmd.target_time_millis;
            let now_millis = self.clock.now_millis();
            let scene_id = scene.scene_id.clone();
            self.runtime.spawn_blocking(move || {
                let outcome = match preparer.lock() {
                    Ok(mut preparer) => preparer
                        .prepare(&scene, &local_files, now_millis)
                        .map(|prepared| PrepareOutcome::Ready {
                            scene: prepared,
                            target_time_millis,
                        })
                        .unwrap_or_else(|err| PrepareOutcome::Failed {
                            scene_id: scene_id.clone(),
                            reason: err.to_string(),
                        }),
                    Err(err) => PrepareOutcome::Failed {
                        scene_id,
                        reason: err.to_string(),
                    },
                };
                let _ = tx.send(outcome);
            });
        }
    }

    /// 매 프레임 수행: 전환 시각 검사(tick) → 결과 회신 → 화면 그리기.
    fn render_frame(&mut self) {
        let mut pending_save: Option<AppSettings> = None;
        if let (Some(ui), Some(window)) = (self.settings_ui.as_mut(), self.window.as_ref()) {
            if let SettingsAction::Save(updated) = ui.update(window, &self.settings) {
                pending_save = Some(updated);
            }
            self.update_cursor_visibility();
        }
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
            if let Some(engine) = &self.engine {
                engine.on_scene_switched(
                    &result.scene_id,
                    result.target_time_millis,
                    result.actual_time_millis,
                );
            }
        }

        if let Some((ui_ptr, window)) = overlay {
            let (device, queue, _) = compositor.gpu_resources();
            let device = device.clone();
            let queue = queue.clone();
            let _ = compositor.render_with_overlay(|encoder, view, width, height| {
                // SAFETY: ui는 render_frame 동안 App이 소유하며, overlay는 동기 호출된다.
                let ui = unsafe { &mut *ui_ptr };
                ui.render(
                    &device,
                    &queue,
                    encoder,
                    view,
                    width,
                    height,
                    &window,
                );
            });
        } else {
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
