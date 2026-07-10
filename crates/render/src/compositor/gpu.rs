//! wgpu GPU 컨텍스트 초기화와 텍스처 업로드 헬퍼.
//!
//! 더블버퍼 전환 로직([`super::DoubleBufferCompositor`])과 분리해
//! GPU 보일러플레이트를 이 파일에 격리한다.

use std::sync::Arc;

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

/// 사각형(quad) 정점 하나 (두 삼각형 구성용).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct Vertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
}

/// 캔버스 픽셀 좌표(top-left 원점)의 요소 rect를 NDC quad 정점으로 변환한다.
///
/// 레이아웃 좌표는 캔버스 해상도 기준이고, 화면(surface)이 다른 크기라면
/// NDC 정규화 과정에서 자동으로 스케일된다.
pub(super) fn quad_vertices(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    canvas_width: u32,
    canvas_height: u32,
) -> [Vertex; 6] {
    let cw = canvas_width.max(1) as f32;
    let ch = canvas_height.max(1) as f32;
    let x0 = (x / cw) * 2.0 - 1.0;
    let x1 = ((x + width) / cw) * 2.0 - 1.0;
    let y0 = 1.0 - (y / ch) * 2.0; // 요소 상단.
    let y1 = 1.0 - ((y + height) / ch) * 2.0; // 요소 하단.
    [
        Vertex {
            position: [x0, y1],
            tex_coords: [0.0, 1.0],
        },
        Vertex {
            position: [x1, y1],
            tex_coords: [1.0, 1.0],
        },
        Vertex {
            position: [x1, y0],
            tex_coords: [1.0, 0.0],
        },
        Vertex {
            position: [x0, y1],
            tex_coords: [0.0, 1.0],
        },
        Vertex {
            position: [x1, y0],
            tex_coords: [1.0, 0.0],
        },
        Vertex {
            position: [x0, y0],
            tex_coords: [0.0, 0.0],
        },
    ]
}

/// 기존 quad를 NDC 좌표계 오프셋만큼 이동한 정점 배열을 만든다.
pub(super) fn translated_vertices(
    vertices: &[Vertex; 6],
    offset_x: f32,
    offset_y: f32,
) -> [Vertex; 6] {
    let mut translated = *vertices;
    for vertex in &mut translated {
        vertex.position[0] += offset_x;
        vertex.position[1] += offset_y;
    }
    translated
}

/// wgpu 디바이스/서피스/파이프라인을 묶은 GPU 컨텍스트.
pub(super) struct GpuContext {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub pipeline: wgpu::RenderPipeline,
    pub sampler: wgpu::Sampler,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuContext {
    /// 창에 연결된 GPU 컨텍스트를 초기화한다.
    ///
    /// 순서: instance → surface → adapter(고성능 우선) → device/queue
    /// → surface 설정(VSync Fifo) → 텍스처 샘플링 파이프라인 구성.
    pub async fn new(window: Arc<Window>, width: u32, height: u32) -> Result<Self> {
        // wgpu 초기화: Windows에서는 D3D12, 그 외 Vulkan/Metal 백엔드를 자동 선택.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("no wgpu adapter"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await?;

        // sRGB 포맷을 우선 선택한다 (이미지 색 재현).
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        // Fifo(VSync) 모드: 프레임 주기가 일정해 정밀 전환 타이밍 판단에 유리하다.
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let bind_group_layout = Self::create_bind_group_layout(&device);
        let pipeline = Self::create_pipeline(&device, &bind_group_layout, format);
        let sampler = Self::create_sampler(&device);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            sampler,
            bind_group_layout,
        })
    }

    /// 요소 rect에 해당하는 quad 정점 버퍼를 만든다 (preload 단계 전용).
    pub fn create_quad_buffer(&self, vertices: &[Vertex; 6]) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("element_quad"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// 기존 텍스처의 픽셀 데이터를 교체한다 (영상 프레임 갱신용).
    /// 크기는 텍스처 생성 시와 동일해야 한다.
    pub fn update_rgba_texture(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// 창 크기 변경 시 surface를 재설정한다.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// RGBA 픽셀 데이터를 GPU 텍스처로 업로드하고 bind group을 만든다.
    /// scene prepare 단계에서 호출된다 (표출 시점 업로드 금지).
    pub fn upload_rgba_texture(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let width = width.max(1);
        let height = height.max(1);
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene_texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture_bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        (texture, bind_group)
    }

    /// 텍스처 + 샘플러 bind group layout을 만든다.
    fn create_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        })
    }

    /// 텍스처 샘플링 렌더 파이프라인을 만든다 (shader.wgsl 사용).
    fn create_pipeline(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline_layout"),
            bind_group_layouts: &[bind_group_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // 레이어 alpha 전환을 위해 알파 블렌딩을 켠다.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    }

    /// 선형 필터링 샘플러를 만든다.
    fn create_sampler(device: &wgpu::Device) -> wgpu::Sampler {
        device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        })
    }
}

/// `#RRGGBB` / `#RRGGBBAA` hex 색상 문자열을 RGBA 배열로 파싱한다.
pub(super) fn parse_hex_color(value: &str) -> Option<[u8; 4]> {
    let s = value.trim_start_matches('#');
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&s[range], 16).ok();
    match s.len() {
        6 => Some([byte(0..2)?, byte(2..4)?, byte(4..6)?, 255]),
        8 => Some([byte(0..2)?, byte(2..4)?, byte(4..6)?, byte(6..8)?]),
        _ => None,
    }
}
