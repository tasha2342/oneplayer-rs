# RTB 스케줄 동작 규격

OnePlayer 1.1.0은 기존 일반 편성 `slots`를 유지하면서 별도
`rtb_slots` 구간을 우선 표출한다. RTB 데이터 또는 에셋에 문제가 있으면
같은 절대 시각의 일반 편성을 fallback으로 사용한다.

백엔드 계약의 완성 예제는
[`api/play_data_v1.1.0.example.json`](api/play_data_v1.1.0.example.json),
필드별 규격은
[`api/play_data_v1.1.0.md`](api/play_data_v1.1.0.md)를 참고한다.

## 전체 처리 흐름

```text
SSP bid 응답
  → CMS가 ext.asset을 play_data.rtb_slots 형식으로 정규화
  → OnePlayer가 일반 타임라인과 RTB 오버레이를 각각 생성
  → 표출 임박 에셋 사전 다운로드
  → RTB image/video prepare
  → 성공: RTB 우선 표출
  → 실패: 같은 시각의 일반 편성 표출
  → 실제 표출된 RTB 장면만 tracking 이벤트 발생
```

플레이어는 SSP의 `/v1/api/bid`를 직접 호출하지 않는다. CMS가 bid 결과를
기존 `play_data`에 정규화해서 전달해야 한다. `adm` VAST XML도 플레이어가
파싱하지 않으며 `ext.asset`과 tracking URL을 구조화해서 전달받는다.

## 최상위 데이터 구조

```json
{
  "api_version": "1.1.0",
  "device_id": "DV-1001",
  "date": "2026-07-21",
  "revision": "20260721-r42",
  "timezone": "Asia/Seoul",
  "slots": [],
  "rtb_slots": [],
  "assets": []
}
```

- `slots`: 기존 일반 편성 및 RTB 실패 시 fallback
- `rtb_slots`: RTB 우선 표출 구간
- `rtb_slots`가 없거나 빈 배열이면 기존 버전과 동일하게 동작한다.
- 잘못된 RTB 슬롯 한 건은 무시하고 일반 편성과 다른 유효 RTB 슬롯은 살린다.

## RTB 슬롯 재생 규칙

1. `start_time`과 `end_time`을 타임존 기준 epoch 시각으로 변환한다.
2. 구간은 `[start_time, end_time)`으로 처리한다.
3. 종료 시각이 시작 시각보다 빠르면 다음 날까지 이어지는 슬롯이다.
4. `items`를 `position` 오름차순으로 정렬한다.
5. 각 item의 `asset.duration_seconds`만큼 순서대로 표출한다.
6. 슬롯 종료까지 전체 item cycle을 반복한다.
7. 마지막 item이 슬롯 끝을 넘으면 슬롯 끝에서 장면을 자른다.
8. RTB 종료 후 일반 편성 시각을 뒤로 밀지 않고 원래 절대 시각으로 복귀한다.

타임라인 메모리를 제한하기 위해 현재 시각 기준 과거 2분부터 미래 30분까지만
장면을 확장한다. CMS revision이 같더라도 확장 window가 부족해지면 현재
시각 기준으로 타임라인을 다시 만든다.

## RTB 슬롯 중첩 규칙

RTB 슬롯은 시작 시각 순으로 처리한다.

- 먼저 시작한 유효 슬롯을 우선한다.
- 시작 시각이 같으면 payload에서 먼저 나온 슬롯을 우선한다.
- 이미 채택된 RTB 슬롯과 겹치는 슬롯 전체를 무시하고 경고 로그를 남긴다.
- 빈 item, 잘못된 에셋 또는 지원하지 않는 형식을 가진 슬롯은 채택하지 않는다.

## 지원 광고 형식

| asset.type | MIME | 표출 방식 |
| --- | --- | --- |
| `video` | `video/mp4` | 기존 FFmpeg decoder와 preroll 사용 |
| `image` | `image/jpeg` | 기존 이미지 텍스처 경로 사용 |
| `image` | `image/png` | 기존 이미지 텍스처 경로 사용 |

RTB item 하나는 단일 전체 화면 요소로 변환한다. 에셋의 `width`, `height`를
레이아웃 기준 해상도로 사용하고 `keep_aspect_ratio=true`로 표출한다.
남는 영역은 검은색 배경으로 처리한다.

다음 조건을 모두 만족해야 유효한 에셋이다.

- 지원하는 type/MIME 조합
- 비어 있지 않은 `bid_id`와 `creative_id`
- `https://` 절대 다운로드 URL
- 0보다 큰 width, height, duration
- `size_bytes`가 있다면 0보다 큰 값

HTML, WebView, VAST XML 직접 재생과 MP4 이외의 영상 코덱은 현재 지원하지
않는다.

## 캐시 키와 다운로드

RTB 에셋은 기존 `AssetStore`를 사용한다.

- `creative_id`의 SHA-256 기반 음수 ID를 RTB 전용 `file_id`로 사용한다.
- `creative_id`를 asset revision으로 사용한다.
- 최종 캐시 키는 기존과 동일한 `{file_id}_{revision}` 형식이다.
- 음수 ID를 사용하므로 일반 CMS 파일 ID 공간과 충돌하지 않는다.

표출 임박 window에서는 일반/fallback 에셋을 먼저 준비한다. RTB 에셋
다운로드나 크기/checksum 검증이 실패하면 일반 타임라인 전체를 실패시키지
않고 해당 RTB 슬롯만 비활성화한다. 미래 에셋은 백그라운드에서 받으며
표출 시점까지 준비되지 않았으면 fallback을 사용한다.

## 일반 편성과 병합

타임라인은 다음 순서로 생성한다.

1. 기존 `slots`로 baseline scene 목록을 만든다.
2. 각 RTB item을 image/video scene으로 만든다.
3. RTB scene과 겹치는 baseline 구간을 잘라낸다.
4. 잘린 위치에 RTB scene을 삽입한다.
5. 모든 scene을 시작 시각 기준으로 정렬한다.

각 RTB scene에는 해당 시작 시각에 원래 표출될 baseline scene 복사본을
fallback으로 보관한다. fallback 장면의 구간은 RTB scene 구간에 맞춘다.

## Fallback 발생 조건

다음 상황에서는 RTB 슬롯 전체를 실패 상태로 표시한다.

- RTB payload가 유효하지 않음
- 지원하지 않는 MIME 또는 asset type
- 다운로드 실패
- size/checksum 검증 실패
- 로컬 캐시 파일 누락
- 이미지 decode 또는 영상 open/preroll 등 prepare 실패

다운로드 단계에서 실패하면 재생 루프가 처음부터 baseline 장면을 선택한다.
prepare 단계에서 실패하면 앱이 엔진에 실패를 알리고 같은 시각의 fallback
장면을 즉시 prepare/switch한다.

한 item의 실패는 같은 `rtb_slot_id` 전체에 적용된다. 실패 상태는 새
타임라인이 적용될 때 초기화된다. fallback 자체도 준비되지 않았다면 현재
화면을 유지하고 오류 로그를 남긴다.

## Tracking 연동

실제 RTB scene 전환이 완료된 경우에만 tracking을 시작한다. fallback 일반
편성에는 RTB tracking URL을 호출하지 않는다.

이벤트별 정확한 시점, 재시도와 종료 처리 규칙은
[`TRACKING_URL.md`](TRACKING_URL.md)를 참고한다.

## 주요 로그

- `ignoring invalid RTB slot`: 슬롯 또는 item 검증 실패
- `ignoring overlapping RTB slot`: 다른 RTB 슬롯과 시간 중첩
- `RTB preload failed; baseline fallback will be used`: 사전 다운로드 실패
- `RTB slot disabled; using baseline fallback`: 재생 시점 에셋 누락
- `switch failed`: 렌더 prepare 또는 화면 전환 실패

운영 시에는 RTB 실패 로그와 일반 편성 전환 로그를 함께 확인해야 실제
fallback 성공 여부를 판단할 수 있다.
