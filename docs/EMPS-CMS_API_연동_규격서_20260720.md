# EMPS-CMS API 연동 규격서

> 본 문서는 SKP-SSP와 EMPS-CMS 간의 옥외 광고 전달 인터페이스에 대한 가이드입니다.
>
> 원문의 필드명, 예시 값, 표기 방식을 유지했습니다. 다만 Markdown 가독성을 위해 들여쓰기와 누락된 JSON 쉼표를 정리했고, 실시간 API 응답의 `adm` 문자열은 별도 XML 코드 블록으로 분리했습니다.

---

## 개요

본 문서는 SKP-SSP와 EMPS-CMS 간의 옥외 광고 전달 인터페이스에 대한 가이드를 제공한다.

우선, EMPS-CMS 디바이스의 아래 정보가 모두 SKP-SSP에 데이터베이스화되었다는 가정을 둔다.

- EMPS-CMS에 등록된 모든 디바이스 매체 정보를 SKP-SSP에 제공한다.
  - 디스플레이 매체 정보
  - 스크린 전면/후면
  - 템플릿 레이아웃 및 해상도
- SKP-SSP는 그룹 ID(`dooh.id`)와 지면 ID(`device.id`) 정보를 EMPS의 디바이스와 매핑하여 전달한다.

---

# 1. 프리캐싱 API 가이드

프리캐싱 API는 제공된 정보에 의해 디바이스와 해상도 정보, 플레이타임과 슬롯 정보를 모두 알고 있으므로 그룹 ID(`dooh.id`)와 프리캐싱 스케줄링 날짜를 입력하는 방식으로 단순화한다.

## 1.1 Precache API 요청

### HTTP 요청

```http
POST /v1/api/precache HTTP/1.1
Host: xxx.domain.co.kr
Content-Type: application/json;charset=utf-8
Accept: application/json;charset=utf-8
Cache-Control: no-cache
X-skp-doohssp-client-id: 123456790abcdefg
X-skp-doohssp-client-secret: abcdefg123456790
X-skp-dooh-timestamp: 1751788800000
X-skp-dooh-request-id: a1b2c3d4-e5f6-7890-abcd-ef1234567890
X-skp-dooh-version: 1.0
```

### 요청 본문

```json
{
  "id": "req-yyyymmddhhmissrand",
  "dooh": {
    "id": "emps-gangnamdaero-1"
  },
  "test": 0,
  "adt": "20260713"
}
```

## 1.2 Precache API 성공 응답

### HTTP 응답

```http
HTTP/1.1 200 OK
Server: Apache-Coyote/1.1
Pragma: no-cache
Expires: Thu, 01 Jan 1970 00:00:00 GMT
Cache-Control: no-cache
Cache-Control: no-store
Content-Type: application/json;charset=utf-8
Content-Length: xxx
```

### 응답 본문

```json
{
  "id": "req-yyyymmddhhmissrand",
  "dooh": {
    "id": "emps-gangnamdaero-1"
  },
  "test": 0,
  "dt": "20260713",
  "devices": [
    {
      "id": "EMPS-D0001_F_T01",
      "crids": [
        "nad-v003-11-000001122334_1920x1080",
        "nad-b002-11-000000998877_1920x1080"
      ]
    },
    {
      "id": "EMPS-D0001_F_T02",
      "crids": [
        "nad-v003-11-000001122334_1920x1080"
      ]
    }
  ],
  "assets": [
    {
      "crid": "nad-v003-11-000001122334_1920x1080",
      "adid": "nad-v003-11-000001122334",
      "url": "https://cdn.dooh.com/creatives/nad-v003-11-000001122334/1920x1080/creative.mp4",
      "width": 768,
      "height": 1024,
      "type": "video",
      "duration": 15,
      "bytes": 20485760
    },
    {
      "crid": "nad-b002-11-000000998877_1920x1080",
      "adid": "nad-b002-11-000000998877",
      "url": "https://cdn.dooh.com/creatives/nad-b002-11-000000998877/1920x1080/creative.jpg",
      "width": 768,
      "height": 1024,
      "type": "image",
      "bytes": 4185320
    }
  ]
}
```

---

# 2. 실시간 API 가이드

이미 알고 있는 디바이스 정보를 토대로 API를 단순화한다.

- `dooh` 객체는 식별자(`ID`)만 수신한다.
- `imp` 객체는 실제 요구하는 콘텐츠에 대한 정보이나, 슬롯 및 플레이리스트 관리 정책에 따라 자동 설정되므로 요청에서 생략한다.

## 2.1 자동 설정 및 입력 항목

| 이름 | 내용 | 값 | 설명 | 비고 |
|---|---|---:|---|---|
| 슬롯 | 스케줄링되는 콘텐츠의 최소 재생 시간 | 15초 | SKP-SSP가 이미 알고 있음 | |
| 플레이리스트 | 동영상/이미지가 재생되는 플레이리스트 최소 시간 | 60초 | SKP-SSP가 이미 알고 있음 | |
| 최대 재생 시간 | 최대 재생 시간은 슬롯 단위로 몇 개를 사용할지 결정 | 30초 | SKP-SSP가 이미 알고 있음 | |
| 템플릿 | 콘텐츠가 재생되는 스크린-템플릿 식별자 | `"EMPS_D001_F_T01"` | SKP-SSP가 이미 알고 있음 | 디바이스 정보와 전/후면 그리고 템플릿 정보로 암시적으로 정의함 |
| `imp.video.mimes` | 재생 콘텐츠 타입 | `"video/mp4"`, `"image/jpeg"` | 비디오/스틸이미지 모두 `video`에 포함함 | 자동 설정 |
| `imp.video.minduration` / `maxduration` | 콘텐츠의 최소/최대 재생 시간 | 15초/30초 | 슬롯 단위 시간 15초와 2배인 30초로 설정 | 자동 설정 |
| `imp.video.w` / `h` | 콘텐츠 사이즈 | 768 / 1024 | 이미 알고 있는 템플릿 사이즈로 설정 | 자동 설정 |
| `imp.video.maxseq` | 플레이리스트에 들어갈 최대 콘텐츠 수 | 4개 | 60초 플레이리스트를 갖는다면 최대 4개임 | 자동 설정 |
| `imp.video.poddur` | 플레이리스트의 총 재생 시간 | 60초 | 최소 15초 슬롯이 4개 들어감 | 자동 설정 |
| `imp.dt` | 예상 재생 시간 | `-` | 요청 메시지의 `dt` 값과 동일하게 처리 | 자동 설정 |
| `imp` 그 외 요소 | `bidfloor`, `qty` 등 생략 | `-` | 값 없음 처리 | 생략 |
| `device.ifa` | 디바이스 식별자 | `"EMPS_D001_F_T01"` | `"(매체)_(디바이스)_(전면/후면)_(템플릿)"` 포맷 구성 | 입력 필요 |
| `device.devicetype` | 디바이스 타입 | `8` | 고정값 | 입력 필요 |
| `device.ifa_type` | 디바이스 식별자 타입 | `"sspid"` | SKP-SSP가 전달한 ID 타입 | 입력 필요 |
| `device` 그 외 요소 | `geo` 등 생략 | `-` | SKP-SSP가 이미 알고 있음 | 생략 |
| `cur` | 통화 | `"KRW"` | 이미 알고 있음 | 생략 |
| `dt` | 예상 재생 시간 | `-` | 재생할 예상 시간(`Unxitime KST`) | 입력 필요 |

## 2.2 실시간 API 요청

### HTTP 요청

```http
POST /v1/api/bid HTTP/1.1
Host: xxx.domain.co.kr
Content-Type: application/json;charset=utf-8
Accept: application/json;charset=utf-8
Cache-Control: no-cache
X-skp-doohssp-client-id: 123456790abcdefg
X-skp-doohssp-client-secret: abcdefg123456790
X-skp-dooh-timestamp: 1751788800000
X-skp-dooh-version: 1.0
```

### 요청 본문

```json
{
  "id": "req-yyyymmddhhmissrand",
  "dooh": {
    "id": "emps-gangnamdaero-1"
  },
  "test": 0,
  "device": {
    "devicetype": 8,
    "ifa": "EMPS_D001_F_T01",
    "ifa_type": "sspid"
  },
  "dt": 1773417600000
}
```

## 2.3 실시간 API 성공 응답 - 동영상

### HTTP 응답

```http
HTTP/1.1 200 OK
Server: Apache-Coyote/1.1
Pragma: no-cache
Expires: Thu, 01 Jan 1970 00:00:00 GMT
Cache-Control: no-cache
Cache-Control: no-store
Content-Type: application/json;charset=utf-8
Content-Length: xxx
```

### 응답 본문

> 원문의 `adm` 필드는 VAST XML 전체를 문자열로 포함한다. 아래 JSON에서는 가독성을 위해 `adm` 값을 별도 XML 블록으로 분리했다.

```json
{
  "id": "req-yyyymmddhhmissrand",
  "cur": "KRW",
  "seatbid": [
    {
      "seat": "skp-ssp",
      "bid": [
        {
          "id": "bid-0003",
          "impid": "1",
          "price": 12000.0,
          "adid": "nad-v003-11-000001122334",
          "crid": "nad-v003-11-000001122334_1920x1080",
          "w": 1920,
          "h": 1080,
          "adm": "<VAST XML 문자열 - 아래 참조>",
          "exp": 1200,
          "mtype": 2,
          "ext": {
            "asset": {
              "type": "video",
              "mime": "video/mp4",
              "url": "https://cdn.dooh.com/creatives/nad-v003-11-000001122334/1920x1080/creative.mp4",
              "crid": "nad-v003-11-000001122334_1920x1080",
              "adid": "nad-v003-11-000001122334",
              "width": 1920,
              "height": 1080,
              "duration": 15,
              "bytes": 20485760,
              "tracking": [
                {
                  "event": "impression",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/impression?q=012345679abcdefg"
                },
                {
                  "event": "start",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/start?q=012345679abcdefg"
                },
                {
                  "event": "firstquartile",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/firstquartile?q=012345679abcdefg"
                },
                {
                  "event": "midpoint",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/midpoint?q=012345679abcdefg"
                },
                {
                  "event": "thirdquartile",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/thirdquartile?q=012345679abcdefg"
                },
                {
                  "event": "complete",
                  "url": "https://api.dooh.com/nad-v003-11-000001122334_1920x1080/complete?q=012345679abcdefg"
                }
              ]
            }
          }
        }
      ]
    }
  ]
}
```

### `adm` 필드의 VAST XML

```xml
<VAST version="3.0">
  <Ad id="nad-v003-11-000001122334">
    <InLine>
      <AdSystem>DOOH-SSP</AdSystem>
      <Impression><![CDATA[
        https://api.dooh.com/nad-v003-11-000001122334_1920x1080/impression?q=012345679abcdefg
      ]]></Impression>
      <Creatives>
        <Creative>
          <Linear>
            <Duration>00:00:15</Duration>
            <TrackingEvents>
              <Tracking event="start"><![CDATA[
                https://api.dooh.com/nad-v003-11-000001122334_1920x1080/start?q=012345679abcdefg
              ]]></Tracking>
              <Tracking event="firstQuartile"><![CDATA[
                https://api.dooh.com/nad-v003-11-000001122334_1920x1080/firstquartile?q=012345679abcdefg
              ]]></Tracking>
              <Tracking event="midpoint"><![CDATA[
                https://api.dooh.com/nad-v003-11-000001122334_1920x1080/midpoint?q=012345679abcdefg
              ]]></Tracking>
              <Tracking event="thirdQuartile"><![CDATA[
                https://api.dooh.com/nad-v003-11-000001122334_1920x1080/thirdquartile?q=012345679abcdefg
              ]]></Tracking>
              <Tracking event="complete"><![CDATA[
                https://api.dooh.com/nad-v003-11-000001122334_1920x1080/complete?q=012345679abcdefg
              ]]></Tracking>
            </TrackingEvents>
            <MediaFiles>
              <MediaFile
                type="video/mp4"
                width="1920"
                height="1080"
              >https://cdn.dooh.com/creatives/nad-v003-11-000001122334/1920x1080/creative.mp4</MediaFile>
            </MediaFiles>
          </Linear>
        </Creative>
      </Creatives>
    </InLine>
  </Ad>
</VAST>
```
