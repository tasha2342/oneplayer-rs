# Tracking URL 동작 규격

OnePlayer 1.1.0은 RTB 광고에 포함된 이벤트별 URL을 재생 및 렌더링과
분리된 비동기 큐에서 호출한다. URL 호출이 느리거나 실패해도 화면 전환과
광고 재생은 중단되지 않는다.

## 지원 이벤트와 발생 시점

| 이벤트 | 발생 시점 |
| --- | --- |
| `impression` | RTB 장면이 컴포지터에서 실제 화면으로 전환된 시점 |
| `start` | `impression` 직후 |
| `firstquartile` | 실제 전환 시점부터 장면 표출 시간의 25% 경과 |
| `midpoint` | 50% 경과 |
| `thirdquartile` | 75% 경과 |
| `complete` | 장면 표출 시간을 정상적으로 모두 채운 시점 |

시간 기준은 스케줄 목표 시각이 아니라
`PlaybackEngine::on_scene_switched`가 받은 **실제 화면 전환 시각**이다.
RTB 슬롯의 마지막 광고가 슬롯 끝에서 잘리면 원본 에셋 시간이 아닌 실제로
배정된 장면 시간을 기준으로 quartile을 계산한다.

영상과 이미지는 같은 규칙을 사용한다. 다만 CMS가 해당 이벤트 URL을 보내지
않으면 호출하지 않는다. 예를 들어 이미지에 `impression`, `start`,
`complete`만 있으면 quartile 호출은 발생하지 않는다.

## 이벤트 종료 규칙

- 다른 장면으로 조기 전환되면 남아 있는 quartile 타이머를 취소한다.
- 실제 표출 시간이 장면 duration보다 짧으면 `complete`를 호출하지 않는다.
- 정상 종료 시 타이머와 다음 장면 전환 콜백 양쪽에서 `complete`가 감지될 수
  있지만 중복 방지 키로 실제 URL 호출은 한 번만 enqueue된다.
- fallback 일반 편성에는 RTB tracking을 발생시키지 않는다.
- 알 수 없는 이벤트명과 HTTP(S)가 아닌 URL은 파싱 단계에서 무시한다.

## 호출 방식

- 메서드: HTTP `GET`
- 요청 본문: 없음
- 별도 인증 헤더: 없음
- 요청 제한 시간: 5초
- 최대 시도 횟수: 최초 요청 포함 3회
- 재시도 간격: 250ms, 500ms 지수 백오프
- 성공 조건: HTTP 2xx

호출은 bounded Tokio 채널을 사용하는 단일 백그라운드 워커에서 처리한다.
큐 용량은 1,024건이며 재생/렌더 경로에서는 `try_send`만 실행한다. 큐가
가득 찼거나 종료 중이면 해당 호출을 버리고 경고 로그를 남긴다.

## 중복 방지

다음 값을 결합한 키를 프로세스 메모리에 보관한다.

```text
(rtb_slot_id, scene_id, event, url)
```

같은 장면에서 동일 이벤트 URL은 한 번만 처리한다. 동일한 creative가 다른
장면 또는 다른 RTB 슬롯에서 다시 재생되면 별개의 노출이므로 다시 호출한다.

중복 상태와 미전송 큐는 디스크에 저장하지 않는다. 앱이 비정상 종료되면
대기 중인 이벤트가 유실될 수 있으며, 재시작 후 이전 이벤트를 재전송하지
않는다.

## 앱 종료

정상적인 창 닫기 요청이 들어오면 다음 순서로 종료한다.

1. 새 재생 작업과 활성 tracking 타이머를 중지한다.
2. tracking 채널에 shutdown 명령을 넣는다.
3. shutdown 명령보다 먼저 enqueue된 요청을 모두 처리한다.
4. 제한 시간 안에 끝나지 않으면 워커를 중단하고 경고 로그를 남긴다.

워커 drain 대기와 join 대기는 각각 최대 8초다.

## 로그

- 성공: `tracking beacon sent` (`debug`)
- 재시도: `tracking beacon failed, retrying` (`warn`)
- 최종 실패: `tracking beacon permanently failed` (`warn`)
- 큐 포화/종료: `tracking queue full or closed` (`warn`)

로그에는 tracking key, 이벤트명, 시도 횟수와 오류가 기록된다. tracking
실패는 광고 재생 성공 여부와 분리해서 판단해야 한다.

## 데이터 예제

```json
{
  "tracking": [
    {
      "event": "impression",
      "url": "https://tracker.example.com/impression"
    },
    {
      "event": "start",
      "url": "https://tracker.example.com/start"
    },
    {
      "event": "firstquartile",
      "url": "https://tracker.example.com/firstquartile"
    },
    {
      "event": "midpoint",
      "url": "https://tracker.example.com/midpoint"
    },
    {
      "event": "thirdquartile",
      "url": "https://tracker.example.com/thirdquartile"
    },
    {
      "event": "complete",
      "url": "https://tracker.example.com/complete"
    }
  ]
}
```

전체 RTB payload는
[`api/play_data_v1.1.0.example.json`](api/play_data_v1.1.0.example.json)을
참고한다.
