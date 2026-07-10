//! 캐시 정리 로직.
//!
//! 3단계로 정리한다:
//! 1. 오래된(30분+) `.part` 임시 파일 삭제
//! 2. 보호 대상이 아니면서 grace(5분)를 지난 `.bin` 파일 삭제
//! 3. 총량이 상한(기본 1GB)을 넘으면 오래된 파일부터 LRU 삭제
//!
//! 보호 대상: 현재/다음 scene + warm window(20분) 안 scene의 에셋
//! (호출자인 엔진이 protected_cache_keys로 전달).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::info;

use crate::config::{DEFAULT_MAX_CACHE_SIZE_BYTES, SCHEDULE_END_GRACE_MS, STALE_PART_MAX_AGE_MS};

use super::fsutil::{delete_with_sidecars, file_age_millis, file_modified_millis};
use super::AssetStore;

/// 캐시 정리 결과 요약 (진단 로그/overlay용).
#[derive(Debug, Clone, Default)]
pub struct CacheCleanupResult {
    /// 삭제된 파일 수.
    pub deleted_files: usize,
    /// 삭제된 총 바이트.
    pub deleted_bytes: u64,
    /// 정리 후 남은 캐시 크기.
    pub remaining_bytes: u64,
}

impl AssetStore {
    /// 캐시를 정리한다. `protected_cache_keys`에 포함된 에셋은 삭제하지 않는다.
    pub async fn cleanup_cache(
        &self,
        protected_cache_keys: &HashSet<String>,
        now_millis: i64,
    ) -> Result<CacheCleanupResult> {
        let mut deleted_files = 0usize;
        let mut deleted_bytes = 0u64;

        // 1단계: 다운로드가 중단된 채 방치된 .part 임시 파일 제거.
        self.delete_stale_part_files(now_millis, &mut deleted_files, &mut deleted_bytes)?;

        // 2단계: 보호 대상이 아니고 grace 기간이 지난 .bin 파일 제거.
        self.delete_expired_bins(
            protected_cache_keys,
            now_millis,
            &mut deleted_files,
            &mut deleted_bytes,
        )?;

        // 3단계: 총량 상한 초과분을 오래된 순(LRU)으로 제거.
        let remaining_bytes =
            self.enforce_size_limit(protected_cache_keys, &mut deleted_files, &mut deleted_bytes)?;

        info!(
            deleted_files,
            deleted_bytes, remaining_bytes, "cache cleanup complete"
        );
        Ok(CacheCleanupResult {
            deleted_files,
            deleted_bytes,
            remaining_bytes,
        })
    }

    /// 30분 이상 방치된 `.part` 임시 파일을 삭제한다.
    fn delete_stale_part_files(
        &self,
        now_millis: i64,
        deleted_files: &mut usize,
        deleted_bytes: &mut u64,
    ) -> Result<()> {
        for entry in fs::read_dir(self.asset_dir())? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".part") && file_age_millis(&path, now_millis) > STALE_PART_MAX_AGE_MS
            {
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(&path).is_ok() {
                    *deleted_files += 1;
                    *deleted_bytes += size;
                }
            }
        }
        Ok(())
    }

    /// 보호 대상이 아니고 마지막 사용 후 grace(5분)가 지난 `.bin`을 삭제한다.
    fn delete_expired_bins(
        &self,
        protected: &HashSet<String>,
        now_millis: i64,
        deleted_files: &mut usize,
        deleted_bytes: &mut u64,
    ) -> Result<()> {
        for entry in fs::read_dir(self.asset_dir())? {
            let path = entry?.path();
            if !is_bin(&path) {
                continue;
            }
            if protected.contains(&cache_key_of(&path)) {
                continue;
            }
            // 최근에 사용한 파일은 grace 기간 동안 유지한다.
            if file_age_millis(&path, now_millis) <= SCHEDULE_END_GRACE_MS {
                continue;
            }
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            if delete_with_sidecars(&path).is_ok() {
                *deleted_files += 1;
                *deleted_bytes += size;
            }
        }
        Ok(())
    }

    /// 캐시 총량이 상한을 넘으면 오래된 파일부터 삭제한다 (LRU).
    /// 정리 후 남은 총 바이트를 반환한다.
    fn enforce_size_limit(
        &self,
        protected: &HashSet<String>,
        deleted_files: &mut usize,
        deleted_bytes: &mut u64,
    ) -> Result<u64> {
        // 현재 .bin 파일 목록과 총량을 집계한다.
        let mut total_bytes = 0u64;
        let mut cache_files = Vec::new();
        for entry in fs::read_dir(self.asset_dir())? {
            let path = entry?.path();
            if is_bin(&path) {
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                total_bytes += size;
                cache_files.push((path, size));
            }
        }

        // 수정 시각(=마지막 사용) 오름차순: 가장 오래된 것부터 삭제 후보.
        cache_files.sort_by_key(|(path, _)| file_modified_millis(path).unwrap_or(0));
        let limit = self.max_cache_size_bytes.max(DEFAULT_MAX_CACHE_SIZE_BYTES);
        for (path, size) in cache_files {
            if total_bytes <= limit {
                break;
            }
            if protected.contains(&cache_key_of(&path)) {
                continue;
            }
            if delete_with_sidecars(&path).is_ok() {
                total_bytes = total_bytes.saturating_sub(size);
                *deleted_files += 1;
                *deleted_bytes += size;
            }
        }
        Ok(total_bytes)
    }
}

/// `.bin` 확장자인지 판별한다.
fn is_bin(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "bin")
}

/// 파일 경로에서 cache_key(확장자 제외 파일명)를 추출한다.
fn cache_key_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}
