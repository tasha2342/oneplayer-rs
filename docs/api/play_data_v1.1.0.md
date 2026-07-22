# Playback Data API v1.1.0

`v1.1.0`은 기존 `play_data` 응답에 RTB 광고 구간을 추가한다. 기존
클라이언트와의 호환성을 위해 `slots` 형식은 변경하지 않고 최상위
`api_version`과 `rtb_slots`만 추가한다.

기준 payload는
[`play_data_v1.1.0.example.json`](play_data_v1.1.0.example.json)이다.
백엔드와 플레이어의 계약 테스트도 이 파일을 사용한다.

## 최상위 필드

- `api_version`: 문자열. RTB 응답은 `"1.1.0"`을 사용한다.
- `slots`: 기존 일반 편성. RTB가 실패할 때 동일 시각의 fallback이다.
- `rtb_slots`: RTB 구간 배열. RTB가 없으면 생략하거나 빈 배열로 보낸다.
- 나머지 필드는 기존 `play_data`와 동일하다.

## RTB 구간

`rtb_slots[]` 필드는 다음과 같다.

- `id` (필수): 하루치 응답 안에서 고유하고 안정적인 RTB 슬롯 ID.
- `start_time`, `end_time` (필수): `HH:MM:SS`. 구간은 `[start, end)`이다.
  종료 시각이 시작보다 빠르면 다음 날까지 이어지는 구간이다.
- `request_id` (선택): SSP bid 요청 ID.
- `currency` (선택): ISO 4217 통화 코드. 기본값은 `KRW`.
- `items` (필수): `position` 오름차순으로 슬롯 종료까지 반복되는 광고 목록.

RTB 슬롯끼리는 겹치지 않아야 한다. 겹친 payload가 오면 먼저 시작한 유효
슬롯만 적용하고 나머지는 무시한다. 동일 시작 시각이면 payload에 먼저 나온
슬롯을 우선한다.

## RTB 항목

- `position` (필수): 슬롯 내 재생 순서.
- `bid_id`, `imp_id`, `ad_id`, `creative_id` (필수): SSP bid 메타데이터.
- `price`, `expires_in_seconds` (선택): 진단용 입찰가와 응답 유효 시간.
- `asset` (필수): 플레이어가 다운로드하고 표출할 단일 광고 에셋.
- `tracking` (선택): 이벤트별 호출 URL. 없으면 트래킹하지 않는다.

`expires_in_seconds`는 SSP 응답을 CMS가 받은 시점 기준의 TTL이며 플레이어의
표출 여부 판단에는 사용하지 않는다. CMS는 만료된 bid를 `play_data`에
포함하지 않아야 한다.

## 광고 에셋

- `type`: `video` 또는 `image`.
- `mime_type`: 현재 `video/mp4`, `image/jpeg`, `image/png`를 지원한다.
- `download_url`: 플레이어가 직접 GET할 수 있는 절대 HTTPS URL.
- `width`, `height`: 원본 픽셀 크기. 양의 정수여야 한다.
- `duration_seconds`: 양의 정수. 슬롯 내 item 확장과 트래킹 진행률 기준이다.
- `size_bytes`, `checksum`: 선택 다운로드 검증값.

RTB 광고는 에셋 비율을 유지한 전체 화면 단일 요소로 표출한다. 지원하지 않는
형식, 잘못된 데이터, 다운로드 또는 prepare 실패 시 해당 RTB 슬롯을 사용하지
않고 같은 절대 시각의 기존 `slots` 편성을 표출한다. RTB 종료 뒤 기존 편성의
시각을 뒤로 밀지 않는다.

## 트래킹

`tracking[]` 항목은 `event`와 절대 `url`을 가진다. 지원 이벤트는 다음과 같다.

- `impression`: 장면이 실제 화면으로 전환된 시점
- `start`: `impression` 직후
- `firstquartile`: 실제 시작 후 광고 duration의 25%
- `midpoint`: 50%
- `thirdquartile`: 75%
- `complete`: 정상적으로 duration을 모두 표출한 시점

플레이어는 payload에 있는 이벤트만 호출한다. 이미지도 전달된 quartile URL이
있으면 체류 시간을 기준으로 호출한다. 조기 전환이나 실패 시 남은 quartile과
`complete`는 호출하지 않는다.

호출은 인증 헤더와 본문 없는 HTTP GET이다. 재생과 분리된 큐에서 5초
타임아웃, 최대 3회 총 시도, 지수 백오프로 처리한다. 같은
`(rtb_slot_id, scene_id, event, url)`은 한 번만 발생시킨다.

## CMS 변환 책임

CMS는 SSP bid 응답의 `seatbid[].bid[]`와 `ext.asset`을 위 snake_case
구조로 정규화한다. `adm` VAST XML은 플레이어에 전달하지 않는다.

- `bid.id` → `bid_id`
- `bid.impid` → `imp_id`
- `bid.adid` → `ad_id`
- `bid.crid` → `creative_id`
- `bid.exp` → `expires_in_seconds`
- `ext.asset.mime` → `asset.mime_type`
- `ext.asset.url` → `asset.download_url`
- `ext.asset.duration` → `asset.duration_seconds`
- `ext.asset.bytes` → `asset.size_bytes`
- `ext.asset.tracking` → `tracking`
