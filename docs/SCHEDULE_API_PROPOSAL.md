# 스케줄 API(play_data) 응답 정규화 제안

- 작성일: 2026-07-09
- 대상 API: `GET /api/v1/devices/{device_id}/play_data?date=YYYY-MM-DD`
- 대상 독자: CMS 백엔드 개발팀
- 목적: 응답 크기 ~99% 축소, 플레이어 파싱/메모리 부담 제거, 파일 캐시 정확도 개선

---

## 1. 현행 응답의 문제 (실측: DV-1001, 2026-07-07 응답 10.1MB)

### 1-1. 동일 데이터의 대량 반복

현행 응답은 하루치 스케줄을 **15초 단위 슬롯으로 서버에서 미리 펼쳐서** 내려준다.
슬롯마다 item 전체와 **레이아웃 정의 전문(全文)이 통째로 반복**된다.

실측 통계:

| 항목 | 값 |
|---|---|
| 응답 크기 | 10,156,435 bytes (10.1MB) |
| `slots[]` 개수 | 5,563개 |
| 슬롯에 내장된 layout 정의 | 5,563회 (매 슬롯 반복) |
| **고유** layout | **6개** (id: 10, 12, 13, 14, 16, 19) |
| **고유** playlist | **2개** (id: 9, 10) |
| **고유** schedule | **4개** (id: 39, 48, 57, 58) |
| **고유** 파일 | **5개** (file_id: 37, 38, 39, 41, 42) |

같은 레이아웃이 평균 **927회** 반복 전송되고 있다. 실질 정보량은 수십 KB다.

부가 문제:

- `playback_data`와 `playback_data_b64`가 같은 내용을 이중으로 담고 있다 (base64 중복).
- 레이아웃 요소에 `created_at`/`updated_at`/`lock` 등 플레이어가 쓰지 않는 필드 포함.
- 매 동기화(5분)마다 10MB를 다시 받는다 → 단말 회선/서버 대역폭 낭비.

### 1-2. 파일별 revision/무결성 정보 부재 (버그 유발)

- 최상위 `assets[]` 배열이 **아예 없다** (실측 0건).
- item의 `file_downloads[]`에 `file_id`와 `download_url`만 있고
  **`revision` / `size_bytes` / `checksum`이 없다.**

플레이어는 파일을 `{file_id}_{revision}.bin`으로 캐시하는데, 파일별 revision이 없어
**스케줄 전체 revision을 대신 사용**할 수밖에 없다. 그 결과:

1. 스케줄 revision이 바뀌면 (편성만 바뀌고 파일 내용은 그대로여도)
   **모든 파일이 새 캐시 키로 재다운로드**된다.
2. 옛 캐시 파일은 정리 대상이 되어 삭제된다.
3. 이 재다운로드/삭제 churn이 2026-07-08 장애(존재하지 않는 옛 revision 경로를
   ffmpeg가 열다 실패 → 스케줄 누락)의 방아쇠였다. (상세: 저장소 루트 `BUG_REPORT.md`)
4. size/checksum이 없어 다운로드 무결성 검증도 불가능하다 (부분 다운로드 감지 못 함).

---

## 2. 제안: 참조 기반 정규화 구조

**"정의는 한 번, 참조는 ID로"** 원칙으로 응답을 4개 섹션으로 분리한다.
`SCHEDULE > PLAYLIST > LAYOUT` 내장(embed) 구조를 → `schedule → playlist_id`,
`playlist → layout_id`, `layout → file_id` **참조** 구조로 바꾼다.

```json
{
  "message": "ok",
  "data": {
    "device_id": "DV-1001",
    "date": "2026-07-07",
    "revision": "569f5c50444d700c09e249f9",
    "server_time": "2026-07-07T00:00:12+09:00",
    "timezone": "Asia/Seoul",

    "assets": [
      {
        "file_id": 37,
        "revision": "a1b2c3d4",            // ★ 파일 내용 기준 revision (내용 변경 시에만 변경)
        "download_url": "/api/v1/content-files/37/file",
        "mime_type": "image/png",
        "size_bytes": 1048576,             // ★ 무결성 검증용
        "checksum": "sha256:9f86d08..."    // ★ 무결성 검증용
      }
    ],

    "layouts": [
      {
        "id": 10,
        "name": "도서관 안내 1",
        "width": 1080,
        "height": 1920,
        "default_duration": 15,
        "elements": [
          { "id": "datetime-1", "type": "system_datetime", "x": 0, "y": 0,
            "width": 1080, "height": 120, "font": "Noto Sans KR", "font_size": 90,
            "background_color": "#d9ffeb", "text_color": "#000000" },
          { "id": "image-1", "type": "image", "x": 0, "y": 106,
            "width": 1080, "height": 1814, "file_id": 37 }
        ]
      }
    ],

    "playlists": [
      {
        "id": 9,
        "name": "테스트 플레이리스트",
        "items": [
          { "id": 91, "position": 0, "layout_id": 10,        // ★ 레이아웃은 ID 참조
            "duration_seconds": 15, "transition": "fade", "loop": true }
        ]
      }
    ],

    "schedule": [
      { "schedule_id": 48, "playlist_id": 9,                 // ★ 플레이리스트는 ID 참조
        "start_time": "00:00:00", "end_time": "08:59:59" },
      { "schedule_id": 57, "playlist_id": 10,
        "start_time": "09:00:00", "end_time": "17:59:59" }
    ]
  }
}
```

### 핵심 변경점

| # | 변경 | 이유 |
|---|---|---|
| 1 | `layouts[]` — 레이아웃을 id별 1회만 정의 | 5,563회 반복 제거. 응답의 대부분을 차지하는 중복 해소 |
| 2 | `playlists[]` — item 목록을 playlist별 1회만 정의, layout은 `layout_id` 참조 | item 반복 제거 |
| 3 | `schedule[]` — 시간 구간 + `playlist_id` 참조만 | **15초 단위 사전 펼침 제거.** 구간 반복 재생 확장은 플레이어가 수행 (이미 구현되어 있음 — 플레이어는 item duration 기반 cycle 확장 로직 보유) |
| 4 | `assets[]` 최상위 복원 + **파일별 `revision`/`size_bytes`/`checksum` 필수** | 파일 내용이 안 바뀌면 재다운로드 없음. 무결성 검증 가능. §1-2 장애 재발 방지 |
| 5 | `playback_data_b64` 제거, `created_at`/`updated_at`/`lock` 등 미사용 필드 제거 | 중복/불용 데이터 제거 |

### 필드 규칙

- `assets[].revision`: **파일 바이너리 내용 기준** 해시 또는 버전. 스케줄 편성이 바뀌어도
  파일 내용이 같으면 유지되어야 한다 (이 값이 단말 캐시 키가 된다).
- `assets[].checksum`: `sha256:<hex>` 형식 권장. `size_bytes`와 함께 다운로드 검증에 사용.
- `schedule[].start_time`/`end_time`: 기존과 동일한 `HH:MM:SS` (단말 타임존 기준).
  `end_time < start_time`이면 자정을 넘는 구간 (기존 규칙 유지).
- `playlists[].items[].duration_seconds`: 0 또는 생략 시 layout `default_duration` → 15초 순.

---

## 3. 기대 효과

| 항목 | 현행 | 제안 후 |
|---|---|---|
| 응답 크기 (실측 데이터 기준) | 10.1MB | **~30KB** (약 99.7% 감소) |
| 서버 → 단말 트래픽 (5분 주기, 단말 1대/일) | ~2.9GB | ~8.6MB |
| 단말 JSON 파싱 | 10MB 문자열 파싱 + 5,563 슬롯 역직렬화 | 즉시 |
| 단말 타임라인 메모리 | scene 5,563개가 각자 레이아웃 사본 보유 | 레이아웃 6개 공유 |
| revision 변경 시 파일 재다운로드 | 전체 파일 (내용 무관) | 내용이 바뀐 파일만 |
| 다운로드 무결성 검증 | 불가 | size + sha256 검증 |

부가 효과: 서버 측 응답 생성도 단순해진다 (슬롯 펼침 로직 제거, 정의 테이블 그대로 직렬화).

---

## 4. 마이그레이션 방안

플레이어 구버전과의 호환을 위해 **신규 엔드포인트 병행**을 권장한다.

1. **Phase 1 — 신규 버전 병행 제공**
   - 기존: `GET /api/v1/devices/{id}/play_data` (현행 유지)
   - 신규: `GET /api/v2/devices/{id}/play_data` 또는 `Accept-Version: 2` 헤더
   - 신규 플레이어는 v2를 우선 호출하고, 404/미지원이면 v1로 폴백.
2. **Phase 2 — 파일별 revision 선행 적용 (v1에도 가능, 하위 호환)**
   - v1 응답의 `file_downloads[]`에 `revision`/`size_bytes`/`checksum`만 먼저 추가해도
     §1-2의 재다운로드 문제와 무결성 검증 부재는 즉시 해소된다.
     기존 플레이어는 추가 필드를 무시하므로 안전하다. **가장 시급하고 저비용.**
3. **Phase 3 — 전 단말 업데이트 후 v1 사전 펼침 응답 폐기.**

### 백엔드 체크리스트

- [ ] (시급) v1 `file_downloads[]`에 `revision`, `size_bytes`, `checksum` 추가
- [ ] v2 응답: `assets[]` / `layouts[]` / `playlists[]` / `schedule[]` 4섹션 구조
- [ ] 슬롯 15초 사전 펼침 제거 (구간 + playlist 참조만 내려주기)
- [ ] `playback_data_b64` 및 미사용 필드(`created_at`, `updated_at`, `lock` 등) 제거
- [ ] 파일 revision은 파일 내용 기준으로 산출 (스케줄 revision과 독립)

---

## 5. 참고

- 플레이어 측 버그 분석: 저장소 루트 `BUG_REPORT.md`
  (옛 revision 캐시 경로 오선택으로 인한 스케줄 누락 — 파일별 revision 부재가 방아쇠)
- 실측 원본: `sample.json` (2026-07-07, DV-1001)
