//! OnePlayer 렌더링 크레이트 (wgpu 기반).
//!
//! `oneplayer-core`가 결정한 "무엇을 언제 표출할지"를 받아
//! "화면에 어떻게 그릴지"를 담당한다.
//!
//! 모듈 개요:
//! - [`compositor`]: 더블버퍼 레이어 전환 (정시 전환의 핵심)
//! - [`layout`]: 레이아웃 좌표 스케일링 + 이미지 preload
//! - [`scene`]: scene 사전 준비 (decode, preroll)
//! - [`image`]: decode된 비트맵 LRU 캐시
//! - [`video`]: 영상 디코더 추상화 (FFmpeg CLI / 스텁)
//! - [`text`]: 텍스트 렌더링 (v2 확장 지점)

pub mod compositor;
pub mod image;
pub mod layout;
pub mod scene;
pub mod text;
pub mod video;

pub use compositor::{DoubleBufferCompositor, SwitchResult};
pub use scene::{PreparedScene, ScenePreparer};
pub use video::{VideoDecoder, VideoFrame};
