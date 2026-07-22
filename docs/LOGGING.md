# API·RTB·Tracking 진단 로그

OnePlayer는 API 연동 실패 지점을 찾을 수 있도록 각 로그에 `api_stage` 필드를
기록한다.

## 로그 파일

기본 경로는 `%LOCALAPPDATA%/OnePlayer/logs`다. `config.toml`의 `data_dir`을
설정하면 `{data_dir}/logs`를 사용한다.

- `oneplayer.log.YYYY-MM-DD`: INFO 이상 전체 실행 로그
- `oneplayer-error.log.YYYY-MM-DD`: WARN/ERROR 전용 로그
- `playback_timing-YYYY-MM-DD.log`: 장면 prepare·전환 타이밍 JSONL

`RUST_LOG=debug`로 실행하면 일반 로그에 HTTP 응답 수신, 캐시 hit, RTB scene
생성 등 DEBUG 진단도 포함된다. 에러 로그는 설정과 관계없이 WARN 이상을
기록한다.

## 장애 추적 순서

아래 `api_stage`를 시간순으로 검색한다.

1. `sync_started`
2. `ntp_sync_completed` 또는 `ntp_sync_failed`
3. `play_data_request`
4. `http_request_started`
5. `http_response_received`
6. `play_data_parsed`
7. `timeline_build_started`
8. `rtb_slot_accepted` 또는 `rtb_*_failed`
9. `asset_download_started`
10. `asset_download_completed`
11. `timeline_apply_completed`
12. `scene_prepare_dispatched`
13. `scene_prepare_completed`
14. `gpu_preload_completed`
15. `scene_switch_completed`
16. `tracking_session_started`
17. `tracking_enqueued`
18. `tracking_http_started`
19. `tracking_http_completed`

중간 단계가 없으면 바로 앞 단계의 WARN/ERROR를 확인한다.

## 주요 실패 단계

| api_stage | 의미 |
| --- | --- |
| `http_transport_failed` | DNS, 연결, TLS 또는 HTTP timeout |
| `http_status_failed` | CMS가 4xx/5xx 반환 |
| `json_parse_failed` | CMS 응답 JSON 구조가 DTO와 불일치 |
| `rtb_dto_parse_failed` | RTB 슬롯 하나의 필수 필드/타입 오류 |
| `rtb_slot_validation_failed` | 빈 ID/items 또는 잘못된 시간 구간 |
| `rtb_item_validation_failed` | MIME, URL, 크기, duration 등 에셋 규칙 위반 |
| `rtb_slot_rejected` | RTB 슬롯 중첩 또는 최종 검증 실패 |
| `rtb_fallback_missing` | RTB 시간에 대응하는 일반 편성이 없음 |
| `asset_download_failed` | 에셋 HTTP/파일 기록 실패 |
| `asset_http_status_failed` | 에셋 서버가 4xx/5xx 반환 |
| `asset_size_mismatch` | CMS metadata와 실제 파일 크기가 다름 |
| `asset_verification_failed` | 다운로드 후 size/checksum/marker 검증 실패 |
| `rtb_preload_failed` | RTB 사전 다운로드 실패, 일반 편성 fallback |
| `scene_prepare_failed` | 이미지 decode 또는 FFmpeg open/preroll 실패 |
| `scene_switch_failed` | prepare 또는 화면 전환 실패 |
| `fallback_asset_not_ready` | RTB와 fallback 에셋이 모두 준비되지 않음 |
| `tracking_session_interrupted` | duration 전에 장면이 종료되어 complete 취소 |
| `tracking_enqueue_failed` | tracking 큐 포화 또는 종료 |
| `tracking_http_retry` | tracking 호출 실패 후 재시도 |
| `tracking_http_failed` | tracking 3회 시도 모두 실패 |

## 상관관계 식별자

- CMS HTTP: `request_id`
- 동기화 주기: `sync_id`
- RTB 요청: `request_id`(CMS payload 값)
- RTB 슬롯: `slot_id` 또는 `rtb_slot_id`
- 광고: `bid_id`, `creative_id`
- 실제 표출 단위: `scene_id`
- 에셋: `file_id`, `cache_key`

동일 `scene_id`를 검색하면 prepare부터 실제 전환과 tracking까지 연결할 수
있다. 다운로드 문제는 scene 로그의 `creative_id`와 에셋 로그의
`cache_key`를 함께 확인한다.

보안상 에셋 및 tracking URL의 query string은 로그에 기록하지 않는다.
CMS 실패 응답 body는 최대 512자만 기록하며 줄바꿈은 제거한다.
