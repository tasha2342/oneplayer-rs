//! 레이아웃 렌더 계획(render plan) 계산.
//!
//! prepare 단계에서 레이아웃 기준 좌표를 실제 캔버스 좌표로 변환하고,
//! 이미지 요소들을 미리 decode한다. 표출 시점에는 이 계산을 하지 않는다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use image::ImageReader;
use oneplayer_core::timeline::{LayoutDefinition, LayoutElement, PlaybackScene};
use tracing::debug;

/// 캔버스 좌표로 변환된 레이아웃 요소 하나.
#[derive(Debug, Clone)]
pub struct RenderElement {
    /// 원본 요소 정의.
    pub element: LayoutElement,
    /// 캔버스 좌표계의 위치/크기 (스케일 적용 완료).
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 요소가 참조하는 로컬 파일 경로 (이미지/영상).
    pub image_path: Option<PathBuf>,
}

/// scene 하나의 렌더 계획: 스케일 변환이 끝난 요소 목록.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    /// 원본 레이아웃 정의.
    pub layout: LayoutDefinition,
    /// 캔버스(실제 표출) 해상도.
    pub canvas_width: u32,
    pub canvas_height: u32,
    /// z_index 오름차순으로 정렬된 요소 목록 (그리기 순서).
    pub elements: Vec<RenderElement>,
}

/// 레이아웃 → 렌더 계획 변환기.
pub struct LayoutRenderer;

impl LayoutRenderer {
    /// 레이아웃 기준 좌표를 캔버스 좌표로 스케일링해 렌더 계획을 만든다.
    ///
    /// 변환 규칙 (Android LayoutRenderer와 동일):
    /// - `scale_x = canvas_width / layout.width`
    /// - `scale_y = canvas_height / layout.height`
    /// - 각 요소의 x/y/w/h에 scale을 곱한다
    /// - z_index 오름차순 정렬 (뒤에 그릴수록 위에 보임)
    pub fn build_plan(
        layout: &LayoutDefinition,
        canvas_width: u32,
        canvas_height: u32,
        local_files: &HashMap<String, PathBuf>,
    ) -> RenderPlan {
        let scale_x = canvas_width as f32 / layout.width.max(1) as f32;
        let scale_y = canvas_height as f32 / layout.height.max(1) as f32;

        let mut elements: Vec<RenderElement> = layout
            .elements
            .iter()
            .map(|el| {
                // file_id로 로컬 캐시 파일을 찾는다.
                // 키 형식이 "{file_id}_{revision}"이므로 접두사로 매칭한다.
                let image_path = el.file_id.and_then(|fid| {
                    local_files
                        .iter()
                        .find(|(k, _)| k.starts_with(&format!("{fid}_")))
                        .map(|(_, p)| p.clone())
                });
                RenderElement {
                    element: el.clone(),
                    x: el.x as f32 * scale_x,
                    y: el.y as f32 * scale_y,
                    width: el.width as f32 * scale_x,
                    height: el.height as f32 * scale_y,
                    image_path,
                }
            })
            .collect();
        elements.sort_by_key(|e| e.element.z_index.unwrap_or(0));

        RenderPlan {
            layout: layout.clone(),
            canvas_width,
            canvas_height,
            elements,
        }
    }

    /// 렌더 계획의 이미지 요소들을 미리 decode한다 (prepare 단계 전용).
    ///
    /// 표시 크기에 맞춰 리사이즈해 메모리를 절약한다
    /// (Android의 downsample 정책에 해당).
    /// 반환: 파일 경로 → decode 완료 RGBA 비트맵.
    pub fn preload_images(plan: &RenderPlan) -> Result<HashMap<String, image::RgbaImage>> {
        let mut images = HashMap::new();
        for el in &plan.elements {
            if el.element.element_type != "image" {
                continue;
            }
            let Some(path) = &el.image_path else {
                continue;
            };
            let key = path.to_string_lossy().to_string();
            // 같은 파일을 여러 요소가 참조해도 decode는 한 번만 한다.
            if images.contains_key(&key) {
                continue;
            }
            debug!(path = %path.display(), "preloading image");
            let img = decode_image(path, el.width as u32, el.height as u32)?;
            images.insert(key, img);
        }
        Ok(images)
    }
}

/// 이미지 파일을 decode하고 표시 크기로 리사이즈한다.
/// 크기가 0이면 (계산 불가) 원본 크기를 유지한다.
fn decode_image(path: &Path, target_w: u32, target_h: u32) -> Result<image::RgbaImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let img = reader.decode()?;
    let rgba = img.to_rgba8();
    if target_w == 0 || target_h == 0 {
        return Ok(rgba);
    }
    // 표시 크기로 리사이즈해 GPU 업로드/메모리 비용을 줄인다.
    let resized = image::imageops::resize(
        &rgba,
        target_w.max(1),
        target_h.max(1),
        image::imageops::FilterType::Triangle,
    );
    Ok(resized)
}

/// scene의 레이아웃으로 렌더 계획을 만든다. 레이아웃이 없으면 `None`.
pub fn scene_render_plan(
    scene: &PlaybackScene,
    canvas_width: u32,
    canvas_height: u32,
    local_files: &HashMap<String, PathBuf>,
) -> Option<RenderPlan> {
    scene
        .layout
        .as_ref()
        .map(|layout| LayoutRenderer::build_plan(layout, canvas_width, canvas_height, local_files))
}
