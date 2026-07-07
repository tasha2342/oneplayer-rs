//! AssetStore 통합 테스트: 준비 확인(is_ready)과 ensure_assets 경로 검증.

use std::fs;
use std::io::Write;

use oneplayer_core::cache::AssetStore;
use oneplayer_core::settings::AppSettings;
use oneplayer_core::timeline::AssetRef;

/// 크기 검증 기반 is_ready와, 이미 준비된 에셋의 ensure_assets 통과를 확인한다.
#[tokio::test]
async fn asset_store_download_and_verify() {
    let dir = std::env::temp_dir().join(format!("oneplayer-cache-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let mut settings = AppSettings::default();
    settings.data_dir = Some(dir.clone());

    let store = AssetStore::new(&settings).expect("asset store");
    let asset = AssetRef {
        file_id: 99,
        revision: "rev".into(),
        download_url: "file:///dev/null".into(),
        mime_type: Some("image/png".into()),
        size_bytes: Some(4),
        checksum: None,
    };

    // Write a fake asset directly for readiness test.
    let path = store.local_path(&asset);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(&[1, 2, 3, 4]).unwrap();
    fs::write(store.asset_dir().join("99_rev.complete"), b"ok").unwrap();
    assert!(store.is_ready(&asset));

    let map = store
        .ensure_assets(std::slice::from_ref(&asset))
        .await
        .expect("ensure");
    assert_eq!(map.len(), 1);
}
