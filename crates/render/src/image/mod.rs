//! decode된 이미지 비트맵의 LRU 메모리 캐시.
//!
//! 정책 (OnePlayer 0.4.0 계승):
//! - 상한 약 64MB
//! - scene 전환 후 약 32MB까지 부분 정리(partial trim)
//! - 같은 이미지를 다음 scene에서 재사용하면 decode 비용을 아낀다

use std::collections::HashMap;
use std::num::NonZeroUsize;

use image::RgbaImage;
use lru::LruCache;

/// 캐시 상한 기본값 (64MB).
const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// 바이트 상한 기반 LRU 이미지 캐시.
pub struct ImageCache {
    /// 키(파일 경로) → 비트맵. LRU 순서를 자동 관리한다.
    cache: LruCache<String, RgbaImage>,
    /// 현재 캐시가 점유한 총 바이트.
    current_bytes: usize,
    /// 총 바이트 상한.
    max_bytes: usize,
}

impl ImageCache {
    /// 지정한 바이트 상한으로 캐시를 만든다.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            // 항목 수 제한은 넉넉히 두고, 실제 제한은 바이트 기준으로 한다.
            cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
            current_bytes: 0,
            max_bytes: max_bytes.max(1024),
        }
    }

    /// 기본 상한(64MB)으로 캐시를 만든다.
    pub fn default_sized() -> Self {
        Self::new(DEFAULT_CACHE_BYTES)
    }

    /// 캐시에서 비트맵을 조회한다 (LRU 순서 갱신됨).
    pub fn get(&mut self, key: &str) -> Option<&RgbaImage> {
        self.cache.get(key)
    }

    /// 비트맵을 캐시에 넣는다.
    /// 상한을 넘으면 가장 오래 안 쓴 항목부터 밀어낸다.
    pub fn insert(&mut self, key: String, image: RgbaImage) {
        let bytes = image_bytes(&image);
        // 새 항목이 들어갈 자리가 생길 때까지 LRU 순으로 제거.
        while self.current_bytes + bytes > self.max_bytes {
            if self.cache.pop_lru().is_none() {
                break;
            }
            self.recompute_bytes();
        }
        // 같은 키를 덮어쓰면 이전 항목의 바이트를 차감한다.
        if let Some(old) = self.cache.put(key, image) {
            self.current_bytes = self.current_bytes.saturating_sub(image_bytes(&old));
        }
        self.current_bytes += bytes;
    }

    /// 여러 비트맵을 한꺼번에 적재한다 (prepare 단계에서 사용).
    pub fn preload(&mut self, images: HashMap<String, RgbaImage>) {
        for (k, v) in images {
            self.insert(k, v);
        }
    }

    /// 목표 크기까지 LRU 순으로 부분 정리한다 (scene 전환 후 호출).
    pub fn partial_trim(&mut self, target_bytes: usize) {
        while self.current_bytes > target_bytes {
            if self.cache.pop_lru().is_none() {
                break;
            }
            self.recompute_bytes();
        }
    }

    /// 캐시를 전부 비운다 (심각한 메모리 압박 시).
    pub fn clear(&mut self) {
        self.cache.clear();
        self.current_bytes = 0;
    }

    /// 현재 점유 바이트를 다시 계산한다 (pop_lru 후 정확성 보장).
    fn recompute_bytes(&mut self) {
        self.current_bytes = self.cache.iter().map(|(_, img)| image_bytes(img)).sum();
    }
}

/// RGBA 비트맵의 메모리 크기(바이트)를 계산한다.
fn image_bytes(img: &RgbaImage) -> usize {
    img.width() as usize * img.height() as usize * 4
}
