//! 설정 UI (v1.1.0): 왼쪽 하단 패널 + 동그란 설정 버튼.
//!
//! 닫힌 상태에서는 작은 원형 버튼이 표시되고(투명 처리 토글이 꺼져 있을 때),
//! 열린 상태에서는 CMS/디바이스/NTP/캔버스 설정을 편집할 수 있다.

use egui_wgpu::ScreenDescriptor;
use egui_winit::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke,
    Vec2,
};
use oneplayer_core::settings::AppSettings;
use wgpu::{CommandEncoder, Device, Queue, TextureFormat, TextureView};
use winit::event::WindowEvent;
use winit::window::Window;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 설정 UI에서 발생한 사용자 액션.
#[derive(Debug, Clone)]
pub enum SettingsAction {
    /// 아무 동작 없음.
    None,
    /// 설정을 저장한다.
    Save(AppSettings),
}

/// egui 기반 설정 오버레이.
pub struct SettingsUi {
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    panel_open: bool,
    draft_cms_base_url: String,
    draft_device_id: String,
    draft_ntp_server: String,
    draft_canvas_width: String,
    draft_canvas_height: String,
    draft_settings_button_transparent: bool,
    status_message: Option<String>,
    last_paint_jobs: Vec<egui::ClippedPrimitive>,
    last_textures_delta: egui::TexturesDelta,
    pointer_active: bool,
}

impl SettingsUi {
    /// 창과 GPU 리소스로 설정 UI를 초기화한다.
    pub fn new(window: &Window, device: &Device, format: TextureFormat) -> Self {
        let egui_ctx = egui::Context::default();
        configure_fonts(&egui_ctx);
        configure_theme(&egui_ctx);

        let viewport_id = egui_ctx.viewport_id();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(device, format, None, 1, false);

        Self {
            egui_ctx,
            egui_state,
            egui_renderer,
            panel_open: false,
            draft_cms_base_url: String::new(),
            draft_device_id: String::new(),
            draft_ntp_server: String::new(),
            draft_canvas_width: String::new(),
            draft_canvas_height: String::new(),
            draft_settings_button_transparent: false,
            status_message: None,
            last_paint_jobs: Vec::new(),
            last_textures_delta: egui::TexturesDelta::default(),
            pointer_active: false,
        }
    }

    /// 현재 설정으로 편집 필드를 동기화한다.
    pub fn sync_from_settings(&mut self, settings: &AppSettings) {
        self.draft_cms_base_url = settings.cms_base_url.clone();
        self.draft_device_id = settings.device_id.clone();
        self.draft_ntp_server = settings.ntp_server.clone();
        self.draft_canvas_width = settings.canvas_width.to_string();
        self.draft_canvas_height = settings.canvas_height.to_string();
        self.draft_settings_button_transparent = settings.settings_button_transparent;
    }

    /// 설정 패널이 열려 있는지 여부.
    pub fn is_panel_open(&self) -> bool {
        self.panel_open
    }

    /// 설정 버튼/패널이 입력을 받아야 하는지 여부 (커서 표시 판단용).
    pub fn wants_pointer(&self, settings: &AppSettings) -> bool {
        self.panel_open || self.pointer_active || !settings.settings_button_transparent
    }

    /// winit 창 이벤트를 egui에 전달한다. egui가 소비했으면 true.
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.egui_state.on_window_event(window, event);
        response.consumed
    }

    /// UI를 갱신하고 사용자 액션을 반환한다.
    pub fn update(&mut self, window: &Window, settings: &AppSettings) -> SettingsAction {
        let raw_input = self.egui_state.take_egui_input(window);
        let mut action = SettingsAction::None;
        let egui_ctx = self.egui_ctx.clone();
        let full_output = {
            let panel_open = &mut self.panel_open;
            let draft_cms_base_url = &mut self.draft_cms_base_url;
            let draft_device_id = &mut self.draft_device_id;
            let draft_ntp_server = &mut self.draft_ntp_server;
            let draft_canvas_width = &mut self.draft_canvas_width;
            let draft_canvas_height = &mut self.draft_canvas_height;
            let draft_settings_button_transparent = &mut self.draft_settings_button_transparent;
            let status_message = &mut self.status_message;

            egui_ctx.run(raw_input, |ctx| {
                action = draw_ui(
                    ctx,
                    settings,
                    panel_open,
                    draft_cms_base_url,
                    draft_device_id,
                    draft_ntp_server,
                    draft_canvas_width,
                    draft_canvas_height,
                    draft_settings_button_transparent,
                    status_message,
                );
            })
        };
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        let pixels_per_point = window.scale_factor() as f32;
        self.last_paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, pixels_per_point);
        self.last_textures_delta = full_output.textures_delta;
        self.pointer_active = self.egui_ctx.wants_pointer_input();
        action
    }

    /// egui를 GPU에 그린다.
    pub fn render(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        view: &TextureView,
        width: u32,
        height: u32,
        window: &Window,
    ) {
        for (id, image_delta) in &self.last_textures_delta.set {
            self.egui_renderer
                .update_texture(device, queue, *id, image_delta);
        }
        for id in &self.last_textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        let pixels_per_point = window.scale_factor() as f32;
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point,
        };
        self.egui_renderer.update_buffers(
            device,
            queue,
            encoder,
            &self.last_paint_jobs,
            &screen_descriptor,
        );

        let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui_render_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        self.egui_renderer.render(
            &mut render_pass.forget_lifetime(),
            &self.last_paint_jobs,
            &screen_descriptor,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_ui(
    ctx: &egui::Context,
    settings: &AppSettings,
    panel_open: &mut bool,
    draft_cms_base_url: &mut String,
    draft_device_id: &mut String,
    draft_ntp_server: &mut String,
    draft_canvas_width: &mut String,
    draft_canvas_height: &mut String,
    draft_settings_button_transparent: &mut bool,
    status_message: &mut Option<String>,
) -> SettingsAction {
    if !*panel_open {
        draw_settings_button(
            ctx,
            settings,
            panel_open,
            draft_cms_base_url,
            draft_device_id,
            draft_ntp_server,
            draft_canvas_width,
            draft_canvas_height,
            draft_settings_button_transparent,
            status_message,
        );
        return SettingsAction::None;
    }

    let mut action = SettingsAction::None;
    let panel_width = 420.0;
    let panel_height = 520.0;
    let margin = 16.0;
    let screen = ctx.screen_rect();

    egui::Area::new(egui::Id::new("settings_panel"))
        .fixed_pos(egui::pos2(margin, screen.max.y - panel_height - margin))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(28, 36, 52))
                .stroke(Stroke::new(1.0, Color32::from_rgb(55, 68, 92)))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(panel_width);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format!("OnePlayer 설정 v{APP_VERSION}"))
                                .font(FontId::proportional(22.0))
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("송출 디바이스 설정")
                                .font(FontId::proportional(14.0))
                                .color(Color32::from_rgb(170, 180, 200)),
                        );
                        ui.add_space(16.0);

                        labeled_text_edit(ui, "CMS URL", draft_cms_base_url);
                        labeled_text_edit(ui, "디바이스 ID", draft_device_id);
                        labeled_text_edit(ui, "NTP 서버", draft_ntp_server);
                        labeled_text_edit(ui, "캔버스 너비", draft_canvas_width);
                        labeled_text_edit(ui, "캔버스 높이", draft_canvas_height);

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("설정 버튼 투명 처리")
                                    .color(Color32::from_rgb(210, 218, 230)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add(toggle_switch(draft_settings_button_transparent));
                                },
                            );
                        });

                        if let Some(msg) = status_message.as_ref() {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(msg.clone())
                                    .color(Color32::from_rgb(120, 220, 160))
                                    .size(13.0),
                            );
                        }

                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("닫기").color(Color32::WHITE))
                                        .fill(Color32::from_rgb(55, 62, 78))
                                        .min_size(Vec2::new(120.0, 36.0)),
                                )
                                .clicked()
                            {
                                *panel_open = false;
                                *status_message = None;
                                sync_draft_from_settings(
                                    settings,
                                    draft_cms_base_url,
                                    draft_device_id,
                                    draft_ntp_server,
                                    draft_canvas_width,
                                    draft_canvas_height,
                                    draft_settings_button_transparent,
                                );
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("저장").color(Color32::WHITE),
                                            )
                                            .fill(Color32::from_rgb(0, 140, 255))
                                            .min_size(Vec2::new(120.0, 36.0)),
                                        )
                                        .clicked()
                                    {
                                        match build_settings(
                                            settings,
                                            draft_cms_base_url,
                                            draft_device_id,
                                            draft_ntp_server,
                                            draft_canvas_width,
                                            draft_canvas_height,
                                            *draft_settings_button_transparent,
                                        ) {
                                            Ok(updated) => {
                                                *status_message = Some("저장되었습니다.".into());
                                                action = SettingsAction::Save(updated);
                                            }
                                            Err(msg) => {
                                                *status_message = Some(msg);
                                            }
                                        }
                                    }
                                },
                            );
                        });
                    });
                });
        });

    action
}

#[allow(clippy::too_many_arguments)]
fn draw_settings_button(
    ctx: &egui::Context,
    settings: &AppSettings,
    panel_open: &mut bool,
    draft_cms_base_url: &mut String,
    draft_device_id: &mut String,
    draft_ntp_server: &mut String,
    draft_canvas_width: &mut String,
    draft_canvas_height: &mut String,
    draft_settings_button_transparent: &mut bool,
    status_message: &mut Option<String>,
) {
    let button_size = 52.0;
    let margin = 16.0;
    let screen = ctx.screen_rect();
    let pos = egui::pos2(margin, screen.max.y - button_size - margin);

    egui::Area::new(egui::Id::new("settings_fab"))
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let visible = !settings.settings_button_transparent;
            let fill = if visible {
                Color32::from_rgba_unmultiplied(30, 40, 58, 210)
            } else {
                Color32::TRANSPARENT
            };
            let stroke = if visible {
                Stroke::new(1.5, Color32::from_rgb(90, 110, 140))
            } else {
                Stroke::NONE
            };

            let button = egui::Button::new(
                RichText::new(if visible { "⚙" } else { " " })
                    .size(22.0)
                    .color(Color32::WHITE),
            )
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(26))
            .min_size(Vec2::splat(button_size));

            if ui.add(button).clicked() {
                *panel_open = true;
                *status_message = None;
                sync_draft_from_settings(
                    settings,
                    draft_cms_base_url,
                    draft_device_id,
                    draft_ntp_server,
                    draft_canvas_width,
                    draft_canvas_height,
                    draft_settings_button_transparent,
                );
            }
        });
}

fn sync_draft_from_settings(
    settings: &AppSettings,
    draft_cms_base_url: &mut String,
    draft_device_id: &mut String,
    draft_ntp_server: &mut String,
    draft_canvas_width: &mut String,
    draft_canvas_height: &mut String,
    draft_settings_button_transparent: &mut bool,
) {
    *draft_cms_base_url = settings.cms_base_url.clone();
    *draft_device_id = settings.device_id.clone();
    *draft_ntp_server = settings.ntp_server.clone();
    *draft_canvas_width = settings.canvas_width.to_string();
    *draft_canvas_height = settings.canvas_height.to_string();
    *draft_settings_button_transparent = settings.settings_button_transparent;
}

fn build_settings(
    base: &AppSettings,
    draft_cms_base_url: &str,
    draft_device_id: &str,
    draft_ntp_server: &str,
    draft_canvas_width: &str,
    draft_canvas_height: &str,
    settings_button_transparent: bool,
) -> Result<AppSettings, String> {
    let canvas_width = draft_canvas_width
        .trim()
        .parse::<u32>()
        .map_err(|_| "캔버스 너비는 숫자여야 합니다.".to_string())?;
    let canvas_height = draft_canvas_height
        .trim()
        .parse::<u32>()
        .map_err(|_| "캔버스 높이는 숫자여야 합니다.".to_string())?;
    if canvas_width == 0 || canvas_height == 0 {
        return Err("캔버스 크기는 0보다 커야 합니다.".to_string());
    }

    let mut updated = base.clone();
    updated.cms_base_url = draft_cms_base_url.trim().to_string();
    updated.device_id = draft_device_id.trim().to_string();
    updated.ntp_server = draft_ntp_server.trim().to_string();
    updated.canvas_width = canvas_width;
    updated.canvas_height = canvas_height;
    updated.settings_button_transparent = settings_button_transparent;

    updated
        .validate()
        .map_err(|err| format!("{err:#}"))
        .map(|_| updated)
}

fn labeled_text_edit(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .font(FontId::proportional(14.0))
                .color(Color32::from_rgb(210, 218, 230)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .desired_width(220.0)
                    .margin(egui::vec2(8.0, 6.0)),
            );
        });
    });
    ui.add_space(10.0);
}

/// iOS 스타일 on/off 토글 스위치.
fn toggle_switch(on: &mut bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| toggle_switch_ui(ui, on)
}

fn toggle_switch_ui(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = egui::vec2(48.0, 26.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let radius = 0.5 * rect.height();
        let track_color =
            Color32::from_rgb(70, 78, 92).lerp_to_gamma(Color32::from_rgb(0, 188, 170), how_on);
        let knob_color = if response.hovered() {
            Color32::from_rgb(250, 252, 255)
        } else {
            Color32::WHITE
        };

        ui.painter().rect(
            rect,
            radius,
            track_color,
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );

        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter().circle(
            center,
            0.72 * radius,
            knob_color,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 25)),
        );
    }

    response
}

fn configure_fonts(ctx: &egui::Context) {
    let Some(font_bytes) = load_korean_font_data() else {
        tracing::warn!("korean UI font not found; labels may render incorrectly");
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("korean".into(), FontData::from_owned(font_bytes).into());
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "korean".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "korean".into());
    ctx.set_fonts(fonts);
}

/// Windows 시스템 폰트에서 한글 지원 폰트를 찾는다.
fn load_korean_font_data() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\malgun.ttf",
        r"C:\Windows\Fonts\malgunsl.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(data) = std::fs::read(path) {
            return Some(data);
        }
    }
    None
}

fn configure_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(28, 36, 52);
    visuals.window_fill = Color32::from_rgb(28, 36, 52);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(40, 48, 64);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(40, 48, 64);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 60, 80);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0, 140, 255);
    visuals.selection.bg_fill = Color32::from_rgb(0, 120, 220);
    ctx.set_visuals(visuals);
}
