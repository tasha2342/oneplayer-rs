# OnePlayer-rs

Windows용 Rust 디지털 사이니지 플레이어. Android OnePlayer(`one_player/`)와 동일한 재생 정책을 따릅니다.

- 실행 파일: `OnePlayerWin.exe`
- 현재 버전: `0.0.1`

## 요구 사항

- Rust stable (1.80+)
- Windows 10/11 (타겟)
- FFmpeg DLL (영상 재생, `ffmpeg` feature)


cargo run -p oneplayer -- config.toml

## 빌드

```bash
cd oneplayer-rs
cp config.example.toml config.toml
cargo build --release -p oneplayer
# 또는 버전명이 붙은 배포물 생성:
make package
```

Windows 배포물:

```text
target/release/OnePlayerWin.exe
dist/OnePlayerWin-v0.0.1.exe   # make package 사용 시
```

FFmpeg DLL은 `PATH`에 있거나 exe와 같은 폴더에 배치합니다.

## 실행

```bash
cargo run -p oneplayer -- config.toml
```

샘플 스케줄 데모 (Android Phase 1 동등):

```bash
cargo run -p oneplayer -- --sample
```

## 아키텍처

```text
crates/core     NTP, CMS, timeline, cache, playback engine (GPU 무관)
crates/render   wgpu double-buffer compositor, layout/image/video
crates/app      winit event loop, 설정, 로깅, Windows 절전 방지
```

## 설정 (`config.toml`)

| 항목 | 기본값 |
| --- | --- |
| device_id | DV-1001 |
| cms_base_url | https://kn.jdone.co.kr/api |
| ntp_server | 101.79.18.207 |
| canvas | 1080 x 1920 |
| fullscreen | true |

`ONEPLAYER_DEVICE_ID` 환경변수가 있으면 `config.toml`의 `device_id`보다 우선합니다.

## 재생 로그

레이아웃이 실제 화면에 표출되고 종료되면 CMS로 재생 로그를 전송합니다. 요청 부하를 줄이기 위해 단건 API가 아니라 `/api/v1/playback-logs/batch`에 배치로 보냅니다.

- `content_type`: `layout`
- `content_id`: 실행된 `layout.id`
- `started_at`, `ended_at`: UTC ISO 8601 문자열
- `completed`: 정상 종료 시 `true`, 타임라인 교체 등으로 중단되면 `false`
- `extra`: `scene_id`, `schedule_id`, `playlist_id`, `item_id` 등 진단 정보

## v2 예정

- 디버그 overlay
- 설정 UI (투명 버튼)
- Task Scheduler / watchdog 자동 재시작
- 영상 preload 고도화
- 화면 방향 설정 UI

## 정책 문서

- [../one_player/OnePlayer-0.4.0-policy.md](../one_player/OnePlayer-0.4.0-policy.md)
- [../one_player/did-app.md](../one_player/did-app.md)
