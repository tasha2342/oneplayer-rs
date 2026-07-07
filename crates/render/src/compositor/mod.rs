//! 더블버퍼 컴포지터: 두 레이어를 유지하고 목표 시각에 alpha만 전환한다.
//!
//! 핵심 원칙 (OnePlayer 0.4.0 정책):
//! - hidden 레이어에 다음 scene을 미리 업로드(preload)해 둔다
//! - 목표 시각(T)에는 레이어 alpha 교체만 수행한다 (무거운 작업 금지)
//! - T-1초부터는 매 렌더 프레임([`DoubleBufferCompositor::tick`])마다
//!   보정 시각을 검사해 첫 도달 프레임에서 전환한다
//! - 영상 scene은 첫 프레임 준비 신호를 기다리되 최대 2초까지만 유예한다
//!
//! 파일 구성:
//! - `mod.rs`: 전환 상태 관리 + 렌더 루프
//! - [`gpu`]: wgpu 초기화/텍스처 업로드 보일러플레이트

mod gpu;

use std::sync::Arc;

use anyhow::Result;
use oneplayer_core::config::{PRECISE_WINDOW_MS, VIDEO_FIRST_FRAME_WAIT_MS};
use tracing::{info, warn};
use winit::window::Window;

use crate::scene::PreparedScene;

use gpu::{parse_hex_color, quad_vertices, GpuContext};

/// 레이어 전환 결과. `delay_millis = actual - target` (목표 ±100ms).
#[derive(Debug, Clone)]
pub struct SwitchResult {
    pub target_time_millis: i64,
    pub actual_time_millis: i64,
    pub delay_millis: i64,
    pub scene_id: String,
}

/// 레이어에 올라간 그리기 단위 하나 (요소 1개 = quad 1개).
struct ElementDraw {
    /// 요소 rect의 NDC quad 정점 버퍼.
    vertex_buffer: wgpu::Buffer,
    /// 텍스처 + 샘플러 bind group.
    bind_group: wgpu::BindGroup,
    /// 요소 텍스처. 영상 요소는 매 프레임 내용이 갱신된다.
    texture: wgpu::Texture,
    /// 텍스처 크기 (영상 프레임 크기 검증용).
    tex_width: u32,
    tex_height: u32,
    /// 영상 요소 여부 (프레임 갱신 대상).
    is_video: bool,
}

/// 화면 레이어 하나. scene의 GPU 리소스와 표시 상태(alpha)를 가진다.
struct Layer {
    /// 이 레이어에 올라간 준비 완료 scene.
    scene: Option<PreparedScene>,
    /// 요소별 그리기 리소스 (그리기 순서 = 요소 z 순서).
    draws: Vec<ElementDraw>,
    /// 표시 상태. 1.0 = 보임, 0.0 = 숨김. 전환은 이 값만 바꾼다.
    alpha: f32,
}

impl Layer {
    /// 빈(숨김) 레이어를 만든다.
    fn empty() -> Self {
        Self {
            scene: None,
            draws: Vec::new(),
            alpha: 0.0,
        }
    }
}

/// 예약된 전환 정보.
struct PendingSwitch {
    /// 전환 목표 시각 (SignageClock 기준 epoch millis).
    target_time_millis: i64,
}

/// 더블버퍼 컴포지터 본체.
pub struct DoubleBufferCompositor {
    /// GPU 컨텍스트 (surface/device/pipeline).
    gpu: GpuContext,
    /// 두 개의 레이어. `active`가 현재 표시 중, 반대쪽이 preload 대상.
    layers: [Layer; 2],
    /// 현재 표시 중인 레이어 인덱스 (0 또는 1).
    active: usize,
    /// 예약된 전환 (없으면 None).
    pending_switch: Option<PendingSwitch>,
    /// 보정 시각 공급자 (SignageClock::now_millis를 주입받는다).
    now_millis: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl DoubleBufferCompositor {
    /// 창에 연결된 컴포지터를 생성한다.
    /// `now_millis`에는 반드시 SignageClock 기반 함수를 주입해야 한다
    /// (시스템 시각을 쓰면 정시 전환이 보장되지 않는다).
    pub async fn new(
        window: Arc<Window>,
        width: u32,
        height: u32,
        now_millis: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self> {
        let gpu = GpuContext::new(window, width, height).await?;
        Ok(Self {
            gpu,
            layers: [Layer::empty(), Layer::empty()],
            active: 0,
            pending_switch: None,
            now_millis,
        })
    }

    /// 창 크기 변경을 GPU surface에 반영한다.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// 준비된 scene을 hidden 레이어에 미리 업로드한다 (alpha=0 유지).
    /// 표출 시점에는 텍스처 업로드가 발생하지 않도록 여기서 모두 끝낸다.
    pub fn preload(&mut self, scene: PreparedScene) {
        let hidden = 1 - self.active;
        self.upload_scene_to_layer(hidden, scene);
        self.layers[hidden].alpha = 0.0;
    }

    /// 목표 시각에 전환을 예약한다. 실제 전환은 [`Self::tick`]이 수행한다.
    pub fn switch_at(&mut self, target_time_millis: i64) {
        self.pending_switch = Some(PendingSwitch { target_time_millis });
    }

    /// 즉시 레이어를 전환한다 (hidden ↔ active alpha 교체만 수행).
    ///
    /// hidden 레이어에 scene이 없으면 아무것도 하지 않는다
    /// (fallback 정책: 준비 안 된 scene으로 전환하지 않고 현재 화면 유지).
    pub fn switch_now(&mut self) -> Option<SwitchResult> {
        let hidden = 1 - self.active;
        let scene_id = self.layers[hidden]
            .scene
            .as_ref()
            .map(|s| s.scene.scene_id.clone())?;

        let actual = (self.now_millis)();
        let target = self
            .pending_switch
            .as_ref()
            .map(|p| p.target_time_millis)
            .unwrap_or(actual);

        // 전환의 실체: alpha 값 교체뿐이다. (정책의 핵심)
        self.layers[hidden].alpha = 1.0;
        self.layers[self.active].alpha = 0.0;

        // 표출이 끝난 이전 scene의 영상 디코드를 중지한다
        // (ffmpeg 프로세스/스레드가 백그라운드에서 계속 돌지 않도록).
        if let Some(old_scene) = &self.layers[self.active].scene {
            if let Some(decoder) = &old_scene.video_decoder {
                if let Ok(mut guard) = decoder.lock() {
                    guard.stop();
                }
            }
        }

        self.active = hidden;
        self.pending_switch = None;

        let result = SwitchResult {
            target_time_millis: target,
            actual_time_millis: actual,
            delay_millis: actual - target,
            scene_id,
        };
        info!(
            scene_id = %result.scene_id,
            delay_millis = result.delay_millis,
            "layer switched"
        );
        Some(result)
    }

    /// 매 렌더 프레임마다 호출되는 정밀 전환 검사.
    ///
    /// 판단 순서:
    /// 1. 예약이 없거나 T-1초(정밀 window) 밖이면 아무것도 안 함
    /// 2. T 도달 전이면 대기 (다음 프레임에 재검사)
    /// 3. T 도달 + hidden scene 준비됨 → 전환
    /// 4. 영상 scene인데 첫 프레임 미준비 → 최대 2초까지 현재 화면 유지 후 강제 전환
    pub fn tick(&mut self) -> Option<SwitchResult> {
        let pending = self.pending_switch.as_ref()?;
        let now = (self.now_millis)();
        let target = pending.target_time_millis;

        // 정밀 window 밖이면 아직 검사할 필요 없음.
        if target - now > PRECISE_WINDOW_MS {
            return None;
        }
        // 목표 시각 도달 전이면 다음 프레임에 재검사.
        if now < target {
            return None;
        }

        let hidden = 1 - self.active;
        let hidden_scene = self.layers[hidden].scene.as_ref()?;

        // 영상 scene은 첫 프레임이 준비될 때까지 잠시 기다린다 (검은 화면 방지).
        if hidden_scene.is_video && !hidden_scene.first_frame_ready() {
            if now <= target + VIDEO_FIRST_FRAME_WAIT_MS {
                return None; // 유예 시간 내 — 현재 화면 유지하며 대기.
            }
            warn!("video first frame timeout, switching anyway");
        }

        self.switch_now()
    }

    /// 현재 레이어 상태를 화면에 그린다 (매 프레임 호출).
    /// alpha가 0인 레이어는 그리지 않는다.
    pub fn render(&mut self) -> Result<()> {
        // 렌더 패스 전에 보이는 레이어의 영상 프레임을 갱신한다.
        self.update_video_frames();

        let output = self.gpu.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        {
            // 배경은 검정으로 클리어 (fallback 최후 단계).
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            pass.set_pipeline(&self.gpu.pipeline);
            // 보이는 레이어의 요소들을 z 순서(draws 순서)대로,
            // 각 요소의 레이아웃 rect 위치에 그린다.
            for layer in &self.layers {
                if layer.alpha <= 0.001 {
                    continue;
                }
                for draw in &layer.draws {
                    pass.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
                    pass.set_bind_group(0, &draw.bind_group, &[]);
                    pass.draw(0..6, 0..1);
                }
            }
        }

        self.gpu.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        Ok(())
    }

    /// 보이는 레이어의 영상 요소 텍스처를 디코더의 최신 프레임으로 갱신한다.
    ///
    /// 디코더가 fps 페이싱을 담당하므로, 새 프레임이 없으면(None)
    /// 이전 프레임 텍스처가 그대로 유지된다.
    fn update_video_frames(&mut self) {
        for layer in &mut self.layers {
            if layer.alpha <= 0.001 {
                continue;
            }
            let Some(scene) = &layer.scene else { continue };
            let Some(decoder) = &scene.video_decoder else {
                continue;
            };
            let frame = {
                let Ok(mut guard) = decoder.lock() else { continue };
                match guard.decode_next_frame() {
                    Ok(frame) => frame,
                    Err(err) => {
                        warn!("video decode failed: {err:#}");
                        continue;
                    }
                }
            };
            let Some(frame) = frame else { continue };
            for draw in layer.draws.iter_mut().filter(|d| d.is_video) {
                if draw.tex_width == frame.width && draw.tex_height == frame.height {
                    self.gpu
                        .update_rgba_texture(&draw.texture, frame.width, frame.height, &frame.rgba);
                } else {
                    // 크기가 달라졌으면 (예: 디코더 교체) 텍스처를 다시 만든다.
                    let (tex, bg) =
                        self.gpu
                            .upload_rgba_texture(frame.width, frame.height, &frame.rgba);
                    draw.texture = tex;
                    draw.bind_group = bg;
                    draw.tex_width = frame.width;
                    draw.tex_height = frame.height;
                }
            }
        }
    }

    /// scene의 표시 리소스를 지정 레이어에 업로드한다.
    ///
    /// - 이미지 요소: prepare 단계에서 decode된 비트맵을 텍스처로 업로드
    /// - 텍스트 요소: 배경색 단색 텍스처 (글리프 렌더링은 v2에서 glyphon으로)
    /// - 영상 요소: 검정 placeholder 텍스처를 만들고, 표출 중에는
    ///   [`Self::update_video_frames`]가 디코더 프레임으로 내용을 갱신한다
    fn upload_scene_to_layer(&mut self, layer_index: usize, scene: PreparedScene) {
        let mut draws = Vec::new();
        let (canvas_w, canvas_h) = (scene.plan.canvas_width, scene.plan.canvas_height);

        for el in &scene.plan.elements {
            let quad = quad_vertices(el.x, el.y, el.width, el.height, canvas_w, canvas_h);
            match el.element.element_type.as_str() {
                "image" => {
                    // prepare 단계에서 decode해 둔 비트맵을 찾아 업로드한다.
                    let Some(path) = &el.image_path else { continue };
                    let key = path.to_string_lossy().to_string();
                    if let Some(img) = scene.images.get(&key) {
                        let (tex, bg) =
                            self.gpu
                                .upload_rgba_texture(img.width(), img.height(), img.as_raw());
                        draws.push(ElementDraw {
                            vertex_buffer: self.gpu.create_quad_buffer(&quad),
                            bind_group: bg,
                            texture: tex,
                            tex_width: img.width(),
                            tex_height: img.height(),
                            is_video: false,
                        });
                    }
                }
                "text" => {
                    // v1: 배경색 단색만 표시. 글리프 렌더링은 v2 범위.
                    if let Some(color) = &el.element.background_color {
                        let rgba = parse_hex_color(color).unwrap_or([30, 30, 30, 255]);
                        let (tex, bg) = self.gpu.upload_rgba_texture(1, 1, &rgba);
                        draws.push(ElementDraw {
                            vertex_buffer: self.gpu.create_quad_buffer(&quad),
                            bind_group: bg,
                            texture: tex,
                            tex_width: 1,
                            tex_height: 1,
                            is_video: false,
                        });
                    }
                }
                "video" => {
                    // 디코더 출력 크기와 동일한 검정 placeholder를 올린다.
                    // 첫 프레임은 전환 직후 update_video_frames가 채운다.
                    if el.image_path.is_none() {
                        continue; // 에셋 미준비 — prepare 단계에서 이미 걸러짐.
                    }
                    let w = (el.width.round().max(2.0)) as u32;
                    let h = (el.height.round().max(2.0)) as u32;
                    let black = vec![0u8; (w * h * 4) as usize];
                    let (tex, bg) = self.gpu.upload_rgba_texture(w, h, &black);
                    draws.push(ElementDraw {
                        vertex_buffer: self.gpu.create_quad_buffer(&quad),
                        bind_group: bg,
                        texture: tex,
                        tex_width: w,
                        tex_height: h,
                        is_video: true,
                    });
                }
                _ => {}
            }
        }

        self.layers[layer_index] = Layer {
            scene: Some(scene),
            draws,
            alpha: 0.0,
        };
    }
}
