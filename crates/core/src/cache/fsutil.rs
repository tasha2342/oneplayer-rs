//! 캐시용 파일 시스템 헬퍼 함수 모음.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use sha2::{Digest, Sha256};

/// 파일의 SHA-256 해시를 소문자 hex 문자열로 계산한다.
/// 대용량 파일을 고려해 8KB 버퍼 스트리밍으로 처리한다.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 서버 checksum 표기를 비교 가능한 형태로 정규화한다
/// (공백 제거, 소문자화, 콜론 구분자 제거).
pub fn normalize_checksum(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(':', "")
}

/// 파일의 수정 시각을 갱신한다 (LRU 정리 순서에 반영하기 위함).
pub fn touch_file(path: &Path) -> Result<()> {
    if path.exists() {
        let file = fs::OpenOptions::new().write(true).open(path)?;
        file.sync_all()?;
    }
    Ok(())
}

/// 파일 수정 시각을 epoch millis로 반환한다. 조회 실패 시 `None`.
pub fn file_modified_millis(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            t.duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        })
}

/// 파일이 마지막으로 수정된 후 경과한 시간(ms)을 계산한다.
pub fn file_age_millis(path: &Path, now_millis: i64) -> i64 {
    now_millis - file_modified_millis(path).unwrap_or(now_millis)
}

/// `.bin` 파일과 그 sidecar(`.complete` marker)를 함께 삭제한다.
pub fn delete_with_sidecars(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    let complete = path.with_extension("complete");
    if complete.exists() {
        fs::remove_file(complete)?;
    }
    Ok(())
}
