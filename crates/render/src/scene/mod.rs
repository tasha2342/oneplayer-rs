//! scene 사전 준비(prepare) 담당.
//!
//! 정책: 표출 시점(T)에 무거운 작업을 하지 않기 위해,
//! T-12초(prepare window)에 미리 다음을 완료한다:
//! - 레이아웃 좌표 스케일 계산 (render plan)
//! - 이미지 파일 decode + 메모리 캐시 적재
//! - 영상 디코더 open + preroll(첫 프레임 디코드) 시작

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use oneplayer_core::timeline::PlaybackScene;
use oneplayer_core::timing_log::{self, TimingValue};

use crate::image::ImageCache;
use crate::layout::{scene_render_plan, RenderPlan};
use crate::video::{VideoDecoder, VideoDecoderPool};

/// scene 전환 후 이미지 캐시를 줄이는 목표 크기 (32MB).
const IMAGE_CACHE_TRIM_TARGET_BYTES: usize = 32 * 1024 * 1024;

/// prepare가 끝난 표출 준비 완료 scene.
/// 컴포지터는 이 데이터를 hidden 레이어에 업로드만 하면 된다.
pub struct PreparedScene {
    /// 원본 scene 정보.
    pub scene: PlaybackScene,
    /// 좌표 스케일이 적용된 렌더 계획.
    pub plan: RenderPlan,
    /// 파일 경로 → decode 완료된 RGBA 비트맵.
    pub images: HashMap<String, image::RgbaImage>,
    /// 영상 파일의 로컬 경로 (영상 scene일 때).
    pub video_path: Option<PathBuf>,
    /// 이 scene에 lease된 영상 디코더 (preroll 완료 상태).
    pub video_decoder: Option<Arc<Mutex<Box<dyn VideoDecoder>>>>,
    /// prepare 완료 시각 (진단용).
    pub prepared_at_millis: i64,
    /// 영상 포함 여부.
    pub is_video: bool,
}

impl std::fmt::Debug for PreparedScene {
    /// 디코더 핸들을 제외한 요약 정보만 출력한다.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedScene")
            .field("scene_id", &self.scene.scene_id)
            .field("is_video", &self.is_video)
            .field("prepared_at_millis", &self.prepared_at_millis)
            .finish()
    }
}

impl PreparedScene {
    /// 영상 첫 프레임이 표출 가능한 상태인지 확인한다.
    /// 컴포지터가 전환 여부를 판단할 때 사용한다 (검은 화면 방지).
    /// 영상이 아닌 scene은 항상 준비 완료로 본다.
    ///
    /// 렌더 스레드(프레임 tick)에서 호출되므로 블로킹 lock을 쓰지 않는다.
    /// 디코더가 다른 스레드에 잡혀 있으면 "아직 준비 안 됨"으로 본다.
    pub fn first_frame_ready(&self) -> bool {
        match &self.video_decoder {
            Some(decoder) => decoder
                .try_lock()
                .map(|d| d.has_first_frame())
                .unwrap_or(false),
            None => true,
        }
    }
}

/// scene prepare 수행자. 이미지 캐시와 영상 디코더 pool을 소유한다.
pub struct ScenePreparer {
    /// 캔버스(표출 대상) 해상도.
    canvas_width: u32,
    canvas_height: u32,
    /// decode된 비트맵의 LRU 메모리 캐시 (상한 64MB).
    image_cache: ImageCache,
    /// 재사용되는 영상 디코더 pool (동시 2개 제한 — OOM 방지).
    video_pool: VideoDecoderPool,
}

impl ScenePreparer {
    /// 캔버스 해상도를 지정해 생성한다.
    pub fn new(canvas_width: u32, canvas_height: u32, ffmpeg_hwaccel: impl Into<String>) -> Self {
        Self {
            canvas_width,
            canvas_height,
            image_cache: ImageCache::default_sized(),
            video_pool: VideoDecoderPool::new(2, ffmpeg_hwaccel),
        }
    }

    /// scene을 표출 가능 상태로 준비한다.
    ///
    /// 처리 순서:
    /// 1. 레이아웃 좌표를 캔버스 해상도로 스케일링 (render plan)
    /// 2. 이미지 요소들의 파일을 decode해 메모리에 적재
    /// 3. 영상 요소가 있으면 디코더 open + preroll 시작
    ///
    /// 실패하면 에러를 반환하고, 호출자는 전환을 포기한다
    /// (fallback: 현재 화면 유지).
    pub fn prepare(
        &mut self,
        scene: &PlaybackScene,
        local_files: &HashMap<i64, PathBuf>,
        now_millis: i64,
    ) -> Result<PreparedScene> {
        // 1. 좌표 스케일 계산.
        let layout_started = Instant::now();
        let plan =
            match scene_render_plan(scene, self.canvas_width, self.canvas_height, local_files) {
                Some(plan) => {
                    timing_log::record(
                        "INFO",
                        6,
                        "LAYOUT_PLAN_DONE",
                        Some(&scene.scene_id),
                        Some(scene.start_time_millis),
                        Some(now_millis),
                        vec![
                            (
                                "duration_ms",
                                TimingValue::from(layout_started.elapsed().as_millis()),
                            ),
                            ("element_count", TimingValue::from(plan.elements.len())),
                        ],
                    );
                    plan
                }
                None => {
                    timing_log::record(
                        "ERROR",
                        "ERROR",
                        "LAYOUT_PLAN_EXCEPTION",
                        Some(&scene.scene_id),
                        Some(scene.start_time_millis),
                        Some(now_millis),
                        vec![("exception", TimingValue::from("scene has no layout"))],
                    );
                    anyhow::bail!("scene has no layout");
                }
            };

        // 2. 이미지 decode (파일 I/O + decode는 여기, 표출 시점 아님).
        let image_started = Instant::now();
        let preloaded = match crate::layout::LayoutRenderer::preload_images(&plan) {
            Ok(preloaded) => {
                timing_log::record(
                    "INFO",
                    7,
                    "IMAGE_PRELOAD_DONE",
                    Some(&scene.scene_id),
                    Some(scene.start_time_millis),
                    Some(now_millis),
                    vec![
                        (
                            "duration_ms",
                            TimingValue::from(image_started.elapsed().as_millis()),
                        ),
                        ("image_count", TimingValue::from(preloaded.len())),
                    ],
                );
                preloaded
            }
            Err(err) => {
                timing_log::record(
                    "ERROR",
                    "ERROR",
                    "IMAGE_PRELOAD_EXCEPTION",
                    Some(&scene.scene_id),
                    Some(scene.start_time_millis),
                    Some(now_millis),
                    vec![
                        ("exception", TimingValue::from(err.to_string())),
                        (
                            "duration_ms",
                            TimingValue::from(image_started.elapsed().as_millis()),
                        ),
                    ],
                );
                return Err(err);
            }
        };
        self.image_cache.preload(preloaded.clone());

        // 3. 영상 준비: 디코더를 pool에서 빌려 open + preroll.
        //    출력 프레임은 요소 표시 크기로 스케일링한다 (GPU 업로드 비용 절감).
        let video_element = plan
            .elements
            .iter()
            .find(|el| el.element.element_type == "video" && el.image_path.is_some());
        let video_path = video_element.and_then(|el| el.image_path.clone());
        // 영상 파일이 실제로 존재하는지 먼저 확인한다.
        // (ffmpeg를 띄우기 전에 걸러야 ENOENT가 hwaccel 실패로 오인되지 않는다.)
        if let Some(path) = &video_path {
            if !path.is_file() {
                let reason = format!("video asset file missing: {}", path.display());
                timing_log::record(
                    "ERROR",
                    "ERROR",
                    "VIDEO_FILE_MISSING",
                    Some(&scene.scene_id),
                    Some(scene.start_time_millis),
                    Some(now_millis),
                    vec![("exception", TimingValue::from(reason.clone()))],
                );
                anyhow::bail!(reason);
            }
        }
        let is_video = video_path.is_some() || scene.has_video();
        let video_decoder = match (&video_path, video_element) {
            (Some(path), Some(el)) => {
                let decoder = self.video_pool.acquire();
                {
                    let mut guard = decoder.lock().expect("decoder lock");
                    let open_started = Instant::now();
                    if let Err(err) = guard.open(
                        path,
                        el.width.round().max(2.0) as u32,
                        el.height.round().max(2.0) as u32,
                        scene.loop_playback,
                    ) {
                        timing_log::record(
                            "ERROR",
                            "ERROR",
                            "FFMPEG_OPEN_EXCEPTION",
                            Some(&scene.scene_id),
                            Some(scene.start_time_millis),
                            Some(now_millis),
                            vec![
                                ("exception", TimingValue::from(err.to_string())),
                                (
                                    "duration_ms",
                                    TimingValue::from(open_started.elapsed().as_millis()),
                                ),
                                ("path", TimingValue::from(path.display().to_string())),
                            ],
                        );
                        return Err(err);
                    }
                    timing_log::record(
                        "INFO",
                        8,
                        "FFMPEG_OPEN_DONE",
                        Some(&scene.scene_id),
                        Some(scene.start_time_millis),
                        Some(now_millis),
                        vec![
                            (
                                "duration_ms",
                                TimingValue::from(open_started.elapsed().as_millis()),
                            ),
                            ("path", TimingValue::from(path.display().to_string())),
                            (
                                "target_width",
                                TimingValue::from(el.width.round().max(2.0) as u64),
                            ),
                            (
                                "target_height",
                                TimingValue::from(el.height.round().max(2.0) as u64),
                            ),
                        ],
                    );

                    let preroll_started = Instant::now();
                    if let Err(err) = guard.preroll() {
                        timing_log::record(
                            "ERROR",
                            "ERROR",
                            "FFMPEG_PREROLL_EXCEPTION",
                            Some(&scene.scene_id),
                            Some(scene.start_time_millis),
                            Some(now_millis),
                            vec![
                                ("exception", TimingValue::from(err.to_string())),
                                (
                                    "duration_ms",
                                    TimingValue::from(preroll_started.elapsed().as_millis()),
                                ),
                            ],
                        );
                        return Err(err);
                    }
                    timing_log::record(
                        "INFO",
                        9,
                        "FFMPEG_PREROLL_DONE",
                        Some(&scene.scene_id),
                        Some(scene.start_time_millis),
                        Some(now_millis),
                        vec![(
                            "duration_ms",
                            TimingValue::from(preroll_started.elapsed().as_millis()),
                        )],
                    );
                }
                Some(decoder)
            }
            _ => None,
        };

        Ok(PreparedScene {
            scene: scene.clone(),
            plan,
            images: preloaded,
            video_path,
            video_decoder,
            prepared_at_millis: now_millis,
            is_video,
        })
    }

    /// 표출이 끝난 scene을 해제한다.
    /// 디코더는 pool이 소유하므로 lease만 끊고,
    /// 이미지 캐시는 목표 크기(32MB)까지 부분 정리한다.
    pub fn release(&mut self, _scene: PreparedScene) {
        self.image_cache.partial_trim(IMAGE_CACHE_TRIM_TARGET_BYTES);
    }

    /// 이미지 캐시 핸들 (메모리 압박 시 외부에서 정리할 수 있게 노출).
    pub fn image_cache(&mut self) -> &mut ImageCache {
        &mut self.image_cache
    }
}
