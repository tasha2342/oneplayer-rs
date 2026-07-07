//! 텍스트 렌더링 (v2 확장 지점).
//!
//! v1에서는 텍스트 요소를 배경색 단색으로만 표시한다
//! ([`crate::compositor`]에서 처리).
//! v2에서 `glyphon`(cosmic-text 기반 wgpu 텍스트)을 이 모듈에 연결해
//! 실제 글리프 렌더링과 디버그 overlay를 구현한다.

/// 텍스트 렌더러 자리표시자. glyphon 연동 시 이 타입에 상태가 들어간다.
pub struct TextRenderer;

impl TextRenderer {
    /// 빈 렌더러를 생성한다.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}
