# 버그 리포트 — 스케줄 누락 / 지연 표출 (engine switch failed)

- 작성일: 2026-07-09 (BUG-5 추가: 2026-07-09 19시)
- 대상 로그: 2026-07-08 08:31 ~ 08:40, 2026-07-09 10:09 (OnePlayerWin)
- 증상: 특정 scene 전환 시각에 화면이 바뀌지 않고 이전 콘텐츠가 유지됨 (스케줄 누락),
  부팅 직후 다음 scene 경계까지 화면이 뜨지 않음 (지연 표출),
  영상 scene 전환 지연(delay 14초) + 창 "응답 없음" (BUG-5, §7)

---

## 1. 로그 타임라인 분석

```text
08:31:39 WARN  ffmpeg stderr: [h264] Failed setup for format d3d11: hwaccel initialisation returned error.
08:40:22.291 WARN  hardware decode failed, falling back to software
                   hwaccel=d3d11va error=ffmpeg exited before first frame (exit code: 0xfffffffe)
08:40:22.356 WARN  ffmpeg stderr: Error opening input file
                   C:\...\OnePlayer\assets\38_24243920c727683b8f499b37.bin.  ← No such file or directory
08:40:22.430 ERROR engine switch failed
                   scene_id=8e0b1aaf927206c0aa30b618:57:106:1783500030000
                   reason=ffmpeg exited before first frame (exit code: 0xfffffffe)
```

핵심 단서 두 가지:

1. **exit code `0xfffffffe` = -2 = ENOENT** — 하드웨어 디코딩 실패가 아니라 **입력 파일이 없어서**
   ffmpeg가 즉시 종료한 것이다. (08:31의 d3d11 hwaccel 초기화 실패는 별개의 경고로,
   소프트웨어 폴백이 정상 동작해 재생에는 영향이 없었다.)
2. **revision 불일치** — scene_id의 revision은 `8e0b1aaf927206c0aa30b618`(현행)인데,
   ffmpeg가 열려던 파일은 `38_24243920c727683b8f499b37.bin`, 즉 **한 세대 전 revision**
   (`24243920...`)의 캐시 파일이다. 이 파일은 캐시 정리(LRU/만료)로 이미 삭제된 상태였다.

즉 "현행 revision 에셋은 디스크에 멀쩡히 있는데, 플레이어가 옛 revision 경로를 ffmpeg에 넘겨서"
전환이 실패했다. 실패한 scene은 재시도되지 않으므로 해당 구간의 스케줄이 통째로 누락됐다.

---

## 2. 근본 원인 (BUG-1) — 에셋 경로 접두사 매칭 + local_files 무한 누적

### 2-1. 접두사 매칭이 임의 revision을 선택

`crates/render/src/layout/mod.rs` (수정 전):

```rust
// 키 형식이 "{file_id}_{revision}"이므로 접두사로 매칭한다.
let image_path = el.file_id.and_then(|fid| {
    local_files
        .iter()
        .find(|(k, _)| k.starts_with(&format!("{fid}_")))
        .map(|(_, p)| p.clone())
});
```

`local_files`는 `HashMap`이라 순회 순서가 비결정적이다. 같은 `file_id`에 대해 revision이
여러 개 쌓여 있으면 (`38_24243920...`, `38_8e0b1aaf...`) **어느 것이 선택될지 보장이 없다.**

### 2-2. local_files는 누적만 되고 정리되지 않음

`crates/core/src/engine/sync.rs` (수정 전):

```rust
self.local_files.lock().await.extend(files);   // revision이 바뀌어도 옛 항목이 남는다
```

revision이 바뀔 때마다 새 cache_key가 추가되지만 옛 key는 제거되지 않는다.
반면 디스크의 실제 파일은 5분 주기 캐시 정리(`cleanup_cache`)가 보호 window 밖이면 삭제한다.
결과적으로 **"맵에는 있지만 디스크에는 없는" 유령 경로**가 계속 늘어난다.

### 2-3. 장애 연쇄

1. 스케줄 revision 변경 → 파일 38이 `38_8e0b1aaf....bin`으로 재다운로드됨 (아래 §5 참고)
2. 옛 파일 `38_24243920....bin`은 캐시 정리로 삭제됨. 그러나 `local_files`에는 항목이 잔존
3. scene prepare 시 접두사 매칭이 하필 옛 경로를 선택
4. ffmpeg가 존재하지 않는 파일을 열다 ENOENT(-2)로 즉시 종료
5. `spawn_with_fallback`이 이것을 "하드웨어 디코드 실패"로 오판 → 소프트웨어로 재시도 → 같은
   파일이 여전히 없으므로 또 실패 (오판 때문에 로그가 hwaccel 문제처럼 보였음)
6. prepare 실패 → `engine switch failed` → 현재 화면 유지 → **스케줄 누락**

재생 루프의 에셋 준비 검사(`scene_assets_ready`)는 **올바른 현행 revision** 파일을 검사해
통과하므로, 검사와 실제 사용 경로가 서로 다른 파일을 보는 불일치가 문제의 본질이다.

### 수정

- 접두사 매칭 제거. scene이 소유한 `asset_refs`에서 `file_id → 정확한 cache_key 경로`를
  계산해 `SwitchCommand`에 담아 prepare에 전달한다 (검사한 파일 = 사용하는 파일 보장).
- 전역 누적 맵(`PlaybackEngine::local_files`)은 아예 제거 — 유령 경로와 무한 증가의
  원인을 구조적으로 없앴다.
- prepare 단계에서 영상 파일 존재를 먼저 확인하고, 없으면 ffmpeg를 띄우지 않고
  명확한 에러로 실패한다.
- `FfmpegCliDecoder::open`에서 입력 파일 존재 검증. 파일 없음은 hwaccel 폴백을 타지 않는다.

---

## 3. BUG-2 — 타임라인 공백 구간에서 영구 정지

`crates/core/src/timeline/models.rs`의 `next_scene` (수정 전):

```rust
pub fn next_scene(&self, now_millis: i64) -> Option<&PlaybackScene> {
    match self.find_scene_index(now_millis) {
        Some(idx) => self.scenes.get(idx + 1),
        None if self.scenes.is_empty() => None,
        None if now_millis < self.scenes[0].start_time_millis => Some(&self.scenes[0]),
        None => None,      // ← 버그: 미래 scene이 있어도 None
    }
}
```

현재 시각이 scene 사이 공백(예: 슬롯 사이 빈 구간, 전환 실패 직후)에 있고 **첫 scene이 과거**이면,
뒤에 미래 scene이 아무리 많아도 `None`을 반환한다. 재생 루프는 `no upcoming scene`으로 5초마다
재시도하지만 조건이 변하지 않으므로 **그날 남은 스케줄 전체가 누락**된다.
`preload_window_assets`(선다운로드)와 캐시 보호 대상 계산도 같은 함수를 쓰므로 함께 오동작한다.

### 수정

partition point(이진 탐색)로 "시작 시각 > now인 첫 scene"을 찾도록 변경. 단위 테스트 추가.

---

## 4. BUG-3 — 부팅/타임라인 교체 직후 현재 scene 미표출

재생 루프(`crates/core/src/engine/playback.rs`)는 `next_scene`만 표출 대상으로 삼는다.
부팅 시각이 어떤 scene의 한가운데면(예: 10:00~10:01 scene 중 10:00:30에 부팅) 그 scene은
건너뛰고 다음 scene 경계까지 검은 화면이 유지된다. 15초 scene이면 최대 15초,
긴 slot이면 **수십 분까지 지연**될 수 있다.

### 수정

재생 루프에 복구 경로(step 2)를 추가: `current_scene(now)`이 존재하고 화면에 표출된
scene(엔진이 추적하는 "마지막 표출 scene_id")과 다르면 즉시 prepare + 전환(target=now)한다.
이미 표출 중인 scene은 다시 전환하지 않으므로 영상이 재시작되지 않는다.

부수 효과: 전환이 실패한 scene(BUG-1 같은 prepare 실패)도 기존에는 영원히 재시도되지
않았지만, 이제 복구 경로가 5초 간격으로 재시도해 자가 치유된다.

---

## 5. BUG-4 — revision 미변경 시 타임라인 확장 window 소진

다중 item 슬롯은 메모리 보호를 위해 `now ± window`(과거 2분/미래 30분)로만 scene을 확장한다
(`TimelineBuilder::build_slot_scenes`). 그런데 `sync_once`는 revision이 같으면 타임라인을
재구성하지 않으므로, revision이 30분 이상 유지되면 **확장된 scene이 소진되어 재생이 멈춘다.**
정책 상수 `TIMELINE_REFRESH_THRESHOLD_MS`(2분)는 정의만 있고 사용처가 없었다 —
갱신 로직이 미구현 상태였다.

### 수정

`sync_once`에서 revision이 같아도 타임라인의 미래 scene 잔량이 임계값(2분) 미만이면
현재 시각 기준으로 재구성하고 재생 루프를 재시작한다. scene_id가
`{revision}:{schedule_id}:{item_id}:{start_millis}`로 결정적이므로 재구성 전후 동일 scene은
같은 id를 가지며, BUG-3 수정의 "마지막 표출 scene_id" 추적과 맞물려 화면 끊김이 없다.

---

## 6. 부수 이슈

### 6-1. config.toml 문법 오류로 설정 전체가 무시됨

`config.toml` 마지막 줄 `RUST_LOG=debug`는 TOML 문법 위반이다(문자열 값은 따옴표 필요).
`AppSettings::load`가 파싱 단계에서 실패하면서 **파일 전체가 무시되고 기본값으로 동작**한다
(기본 CMS URL `https://kn.jdone.co.kr/api`, 캔버스 1080x1920 등 — 운영 설정과 다름).
release 빌드는 `windows_subsystem = "windows"`라 `eprintln!` 경고도 보이지 않는다.
→ 해당 라인 제거.

### 6-2. 하드웨어 디코드 폴백이 파일 없음 오류를 가림

`spawn_with_fallback`은 첫 프레임 실패의 원인을 구분하지 않고 무조건 소프트웨어로 재시도한다.
파일 자체가 없으면 재시도도 반드시 실패하며(최대 8초 x 2 낭비), 로그에는
"hardware decode failed"가 남아 원인 분석을 방해했다.
→ `open()`에서 파일 존재를 검증해 hwaccel과 무관한 실패를 사전에 분리.

### 6-3. 스케줄 revision 변경 시 전체 에셋 재다운로드 (서버 개선 필요)

현행 play_data 응답에는 최상위 `assets` 배열이 없고 item의 `file_downloads`에도
파일별 `revision`/`size_bytes`/`checksum`이 없다. 이 때문에 캐시 키가
`{file_id}_{스케줄 revision}`으로 만들어져, **파일 내용이 그대로여도 스케줄 revision이 바뀌면
전 파일이 재다운로드**되고 옛 캐시는 삭제 대상이 된다(§2 장애의 방아쇠).
클라이언트 수정으로 유령 경로 문제는 해결되지만, 불필요한 재다운로드 자체는
서버가 파일별 revision/checksum을 내려줘야 근본 해결된다.
→ `docs/SCHEDULE_API_PROPOSAL.md` 참고.

---

## 7. BUG-5 — 영상 전환 14초 지연 + 창 "응답 없음" (2026-07-09 10:09 발생)

### 7-1. 로그 타임라인

```text
10:09:41.220 WARN  ffmpeg produced no first frame
10:09:41.221 WARN  ffmpeg stderr: [h264] Failed setup for format d3d11: hwaccel initialisation returned error.
10:09:41.283 WARN  hardware decode failed, falling back to software
                   hwaccel=d3d11va error=ffmpeg first frame timeout
10:09:44.535 INFO  layer switched scene_id=7bc3ba3f...:57:146:...  delay_millis=14308
10:09:45.089 INFO  switch command dispatched scene_id=...:57:148:...
```

- scene 146의 목표 전환 시각은 10:09:31, 실제 전환은 10:09:45.3 → **14.3초 지연**.
- 같은 시점에 플레이어 창이 Windows "응답 없음" 상태가 됨.
- 오류의 성격이 이전(BUG-1, ENOENT 즉시 종료)과 다르다:
  `first frame timeout` = ffmpeg 프로세스가 **살아있는 채로 8초 대기 한도를 소진**.

### 7-2. 원인 1 — hwaccel 실패를 scene마다 8초씩 다시 기다림 (지연의 주범)

이 장비는 d3d11va 초기화가 항상 실패한다 (`Failed setup for format d3d11`).
그런데 실패 시 ffmpeg가 즉시 죽지 않고 멈춰 있어, preroll이 **첫 프레임 대기 한도
8초를 전부 소진한 뒤에야** 소프트웨어로 재시도했다.

- 영상 preroll은 T-8초에 시작하므로: hw 대기 8초 → 이미 목표 시각 도달,
  + 소프트웨어 ffmpeg 기동/디코드 ~3초 → **delay 14308ms**와 정확히 일치.
- 폴백 학습(`self.hwaccel = ""`)이 **디코더 인스턴스별**로만 저장되어,
  pool의 다른 slot이나 새 디코더는 같은 실패를 또 8초씩 반복했다.

**수정** (`crates/render/src/video/mod.rs`):

- hw 시도의 첫 프레임 대기 한도를 8초 → **3초**로 분리 (`HW_FIRST_FRAME_TIMEOUT`).
  hwaccel 초기화 실패는 수백 ms 안에 드러나므로 충분하다. 소프트웨어는 8초 유지.
- hw 실패를 **pool 전체가 공유하는 플래그**(`hw_disabled: Arc<AtomicBool>`)에 기록.
  한 번 실패하면 이후 모든 디코더/세션이 처음부터 소프트웨어로 디코딩한다.
  → 최악 지연이 "매 영상 scene 8초+"에서 "**앱 실행당 1회, 최대 3초**"로 줄어든다.

### 7-3. 원인 2 — 렌더 스레드가 디코더 mutex에 블로킹 (응답 없음의 주범)

렌더(winit) 스레드는 매 프레임 다음 경로에서 디코더 mutex를 **블로킹 lock**으로 잡았다:

- `update_video_frames()` — 표시 중인 영상 프레임 갱신 (`decoder.lock()`)
- `PreparedScene::first_frame_ready()` — 전환 tick의 첫 프레임 준비 검사 (`lock()`)
- `switch_now()` — 이전 scene 디코더 정지 (`lock()`)

한편 prepare(spawn_blocking 스레드)는 `open()+preroll()` 동안 — 이번 사례처럼
hw 8초 + sw 재시도까지 **십수 초** — 같은 mutex를 잡고 있을 수 있다.

여기에 디코더 pool(slot 2개)의 **라운드 로빈 대여**가 겹치면: 이전에 prepare가
실패한 scene이 있으면 대여 순서(parity)가 밀려, **지금 화면에서 재생 중인 scene이
쓰는 slot을 다음 scene의 prepare에 다시 빌려준다.** 그 순간:

1. prepare의 `open()`이 재생 중인 ffmpeg 세션을 중단시키고 (화면 영상 정지)
2. prepare가 mutex를 십수 초 점유
3. 렌더 스레드가 `update_video_frames()`의 `lock()`에서 그대로 멈춤
   → **메시지 루프 정지 → Windows "응답 없음"** + 전환 tick도 못 돌아 지연 가중

**수정**:

- 렌더 스레드의 모든 디코더 접근을 `try_lock()`으로 변경
  (`compositor/mod.rs`, `scene/mod.rs`). 잡혀 있으면 그 프레임만 건너뛰고
  마지막 텍스처를 유지한다. **렌더 스레드는 이제 어떤 경우에도 블로킹하지 않는다.**
- pool 대여 방식을 라운드 로빈 → **lease 검사**로 변경 (`video/mod.rs`):
  pool만 참조 중인(`Arc::strong_count == 1`) slot만 빌려주고, 전부 사용 중이면
  임시 디코더를 새로 만든다. 재생 중인 scene의 디코더를 뺏는 일이 구조적으로 불가능해졌다.
- lease 반납 명시화 (`compositor/mod.rs`): scene 전환/preload 덮어쓰기 시
  이전 scene의 디코더를 정지하고 `Arc`를 drop해 slot을 회수한다
  (유휴 ffmpeg 프로세스 잔류 방지 — 메모리 관리에도 기여).

### 7-4. 수정 후 기대 동작

| 상황 | 수정 전 | 수정 후 |
|---|---|---|
| hw 디코드 실패 장비의 영상 scene | scene마다 8초+ 대기, 전환 지연 누적 | 첫 1회만 최대 3초, 이후 즉시 소프트웨어 |
| prepare가 길어질 때 UI | 창 "응답 없음" 가능 | 항상 응답 (프레임 갱신만 건너뜀) |
| 디코더 slot 충돌 | 재생 중 영상 정지 + UI 정지 | 발생 불가 (lease 검사 + 임시 디코더) |

참고: 근본적으로 이 장비에서는 d3d11va가 동작하지 않으므로, `config.toml`의
`ffmpeg_hwaccel = "d3d11va"`를 `"none"`으로 바꾸면 첫 1회의 3초 대기도 없앨 수 있다
(공유 플래그가 런타임에 같은 효과를 내지만, 설정이 더 확실하다).

---

## 8. 메모리 문제

| 항목 | 문제 | 수정 |
|---|---|---|
| `local_files` | revision 변경마다 누적, 정리 없음 (유령 경로의 원인이기도 함) | 전역 맵 제거, scene별 file_id→경로 맵으로 대체 |
| `dispatched` (재생 루프) | scene_id가 무한 누적 (15초 scene 기준 하루 ~5,700개/세대) | 종료 시각이 지난 scene 주기 제거 |
| `PlaybackScene.layout` | scene 5,563개가 각자 레이아웃 전체를 복제 — 실제 고유 레이아웃은 6개 | `Arc<LayoutDefinition>`로 layout id 기준 공유 |

sample.json(10.1MB) 기준: 슬롯 5,563개 x 레이아웃 정의 복제였던 것이 고유 6개 공유로 줄어,
타임라인 메모리가 수백 MB급 → 수 MB급으로 감소한다.

---

## 9. 수정 파일 요약

| 파일 | 내용 |
|---|---|
| `crates/render/src/layout/mod.rs` | 접두사 매칭 제거, file_id 정확 매칭, 이미지 decode 실패 시 경로 포함 에러 |
| `crates/render/src/scene/mod.rs` | prepare 전 영상 파일 존재 검증, `first_frame_ready` try_lock화 |
| `crates/render/src/video/mod.rs` | `open()` 입력 파일 존재 검증, hw 대기 3초 분리 + 공유 실패 플래그, pool lease 검사 |
| `crates/render/src/compositor/mod.rs` | 렌더 스레드 try_lock화, scene 전환/preload 시 디코더 정지 + lease 반납 |
| `crates/core/src/timeline/models.rs` | `next_scene` 공백 구간 수정 (partition point), layout Arc화, 회귀 테스트 |
| `crates/core/src/timeline/builder.rs` | 레이아웃 dedupe (id 기준 Arc 공유) |
| `crates/core/src/engine/playback.rs` | 복구 경로(현재 scene 즉시 표출 + 실패 재시도), dispatched 정리, scene별 파일 매핑 |
| `crates/core/src/engine/sync.rs` | 타임라인 잔량/날짜 기반 주기 갱신 (`apply_timeline` 공통화) |
| `crates/core/src/engine/mod.rs` | 마지막 표출 scene_id 추적, 전역 local_files 제거 |
| `crates/core/src/engine/state.rs` | `SwitchCommand.local_files`를 file_id 키 정확 매핑으로 변경 |
| `config.toml` | 잘못된 `RUST_LOG=debug` 라인 제거 |
| `Cargo.toml` | serde `rc` feature (Arc 필드 직렬화) |

검증: `cargo build` 성공, `cargo test --workspace` 19개 전부 통과
(BUG-2 회귀 테스트 5건 포함). BUG-5 수정 후에도 동일하게 전부 통과.
