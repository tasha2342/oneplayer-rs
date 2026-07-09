# OnePlayerWin 설치 매뉴얼 (v0.0.1)

Windows 10/11용 디지털 사이니지 플레이어 설치·배포 가이드입니다.

---

## 1. 요구 사항

| 항목 | 내용 |
| --- | --- |
| OS | Windows 10 / 11 (64비트) |
| GPU | DirectX 11 이상 (화면 렌더링 + `d3d11va` 하드웨어 디코딩) |
| 네트워크 | CMS 서버 접속, NTP 동기화 |
| FFmpeg | **필수** — 영상 재생에 사용 (아래 3절 참고) |

---

## 2. 배포 폴더 구성

권장 배포 디렉터리 예시 (`C:\OnePlayer\`):

```text
C:\OnePlayer\
├── OnePlayerWin.exe      ← 릴리즈 빌드 실행 파일
├── config.toml           ← 단말 설정 (필수)
├── ffmpeg.exe            ← FFmpeg (필수, 영상 재생)
├── ffprobe.exe           ← 메타데이터 조회 (권장)
├── avcodec-61.dll         ┐
├── avformat-61.dll         │
├── avutil-59.dll           │ FFmpeg 동반 DLL (필수)
├── avfilter-10.dll         │ exe와 같은 폴더에 모두 복사
├── avdevice-61.dll         │
├── swresample-5.dll        │
├── swscale-8.dll           │
└── postproc-58.dll         ┘
```

> **중요:** `OnePlayerWin.exe`만 복사하면 **영상이 재생되지 않습니다** (검은 화면).
> FFmpeg 실행 파일과 DLL을 **반드시 exe와 같은 폴더**에 함께 배치하세요.

### 빌드 산출물 위치

```powershell
cd oneplayer-rs
cargo build --release -p oneplayer
# 생성: target\release\OnePlayerWin.exe
```

개발 PC에 이미 있는 FFmpeg 번들:

```text
oneplayer-rs\tools\ffmpeg\   ← 이 폴더 전체를 배포 폴더로 복사
```

PowerShell 예시:

```powershell
$dest = "C:\OnePlayer"
New-Item -ItemType Directory -Force -Path $dest
Copy-Item target\release\OnePlayerWin.exe $dest\
Copy-Item config.example.toml $dest\config.toml
Copy-Item tools\ffmpeg\* $dest\
```

---

## 3. FFmpeg 설치

OnePlayer는 FFmpeg를 **CLI 서브프로세스**로 호출합니다. 시스템 전역 설치가 아니어도 되며, exe 옆에 두는 방식을 권장합니다.

### 방법 A — 프로젝트 번들 복사 (권장)

1. `tools\ffmpeg\` 폴더의 **모든 파일**을 `OnePlayerWin.exe`와 같은 폴더에 복사
2. `ffmpeg.exe`가 있는지 확인

### 방법 B — 공식 빌드 다운로드

1. [https://www.gyan.dev/ffmpeg/builds/](https://www.gyan.dev/ffmpeg/builds/) 에서 **release full** 또는 **release essentials** 빌드 다운로드
2. 압축 해제 후 `bin\` 폴더 안의 `ffmpeg.exe`, `ffprobe.exe` 및 `*.dll`을 배포 폴더로 복사

### 방법 C — 환경 변수로 경로 지정

exe 옆이 아닌 다른 위치에 FFmpeg를 둘 경우:

```powershell
[System.Environment]::SetEnvironmentVariable(
    "ONEPLAYER_FFMPEG_DIR", "D:\ffmpeg\bin", "Machine"
)
```

### FFmpeg 탐색 우선순위 (앱 내부 동작)

1. `ONEPLAYER_FFMPEG_DIR` 환경 변수
2. `OnePlayerWin.exe`와 **같은 폴더**
3. **현재 작업 디렉터리**의 `tools\ffmpeg\`
4. 시스템 `PATH`

---

## 4. 설정 파일 (`config.toml`)

`OnePlayerWin.exe`를 실행할 때 **현재 작업 디렉터리**에 `config.toml`이 있어야 합니다.
없으면 기본값으로 새 파일을 생성하지만, CMS·단말 ID 등은 직접 수정해야 합니다.

`config.example.toml`을 복사해 편집:

```toml
device_id = "DV-1001"                        # CMS에 등록된 단말 ID
cms_base_url = "https://kn.jdone.co.kr/api"
auth_token = ""                              # 필요 시 인증 토큰
ntp_server = "101.79.18.207"

# canvas_width = 1080
# canvas_height = 1920
# fullscreen = true
# ffmpeg_hwaccel = "d3d11va"   # none, d3d11va, dxva2, cuda, qsv 등
```

| 항목 | 설명 | 기본값 |
| --- | --- | --- |
| `device_id` | CMS 단말 식별자 | DV-1001 |
| `cms_base_url` | 스케줄·에셋 API 주소 | (예시 URL) |
| `ntp_server` | 시간 동기화 서버 | 101.79.18.207 |
| `canvas_width` / `canvas_height` | 출력 해상도 | 1080 × 1920 |
| `fullscreen` | 전체화면(borderless) | true |
| `ffmpeg_hwaccel` | HW 디코딩 | `d3d11va` (Windows) |

데이터·캐시·로그 저장 위치 (기본):

```text
%LOCALAPPDATA%\OnePlayer\
├── assets\              ← 다운로드된 미디어 캐시
├── logs\                ← oneplayer.log.YYYY-MM-DD
├── playback_cache.json
└── clock_state.json
```

---

## 5. 실행

### 정상 실행 (CMS 스케줄)

```powershell
cd C:\OnePlayer
.\OnePlayerWin.exe
# 또는 설정 파일 경로 지정:
.\OnePlayerWin.exe C:\OnePlayer\config.toml
```

### 샘플 데모 (네트워크 없이 동작 확인)

```powershell
cd C:\OnePlayer
.\OnePlayerWin.exe --sample
```

### 바로가기 만들 때 주의

탐색기에서 exe를 더블클릭하면 **작업 디렉터리가 exe 위치**가 됩니다.
이 경우 `config.toml`과 `ffmpeg.exe`(+DLL)가 exe **와 같은 폴더**에 있어야 합니다.

바로가기 속성 → **시작 위치**를 `C:\OnePlayer`로 지정하는 것을 권장합니다.

---

## 6. `cargo run`은 되는데 exe만 영상이 안 나올 때

가장 흔한 원인입니다.

| | `cargo run` (개발) | `OnePlayerWin.exe` (배포) |
| --- | --- | --- |
| 작업 디렉터리 | 프로젝트 루트 `oneplayer-rs\` | exe를 실행한 위치 |
| FFmpeg 경로 | `oneplayer-rs\tools\ffmpeg\` 자동 인식 | **exe 옆 또는 PATH에 없으면 실패** |
| 영상 없을 때 | — | 검정 화면 (스텁 디코더로 폴백) |

**해결:** 배포 폴더에 `ffmpeg.exe`와 DLL 전부를 `OnePlayerWin.exe` 옆에 복사하세요.

### 동작 확인

```powershell
cd C:\OnePlayer
.\ffmpeg.exe -version          # FFmpeg 정상 여부
$env:RUST_LOG = "info"
.\OnePlayerWin.exe --sample    # 콘솔에 "ffmpeg found" 로그 확인
```

로그 파일 확인:

```text
%LOCALAPPDATA%\OnePlayer\logs\oneplayer.log.YYYY-MM-DD
```

| 로그 메시지 | 의미 |
| --- | --- |
| `ffmpeg found` | FFmpeg 인식 성공 |
| `ffmpeg unavailable, falling back to stub` | **FFmpeg 미발견 → 영상 재생 불가** |
| `ffmpeg produced no first frame` | 파일 손상·코덱·HW 가속 문제 |

HW 디코딩 문제 시 `config.toml`에서 소프트웨어 디코딩으로 전환:

```toml
ffmpeg_hwaccel = "none"
```

---

## 7. 자동 시작 (선택)

Windows 작업 스케줄러로 로그온 시 자동 실행:

1. **작업 스케줄러** → 기본 작업 만들기
2. 트리거: **사용자 로그온 시**
3. 동작: 프로그램 시작
   - 프로그램: `C:\OnePlayer\OnePlayerWin.exe`
   - 시작 위치: `C:\OnePlayer`
4. **가장 높은 수준의 권한으로 실행** (필요 시)

---

## 8. 문제 해결 체크리스트

- [ ] `OnePlayerWin.exe`와 `ffmpeg.exe`(+DLL)가 **같은 폴더**에 있는가?
- [ ] `config.toml`이 실행 위치(또는 지정 경로)에 있는가?
- [ ] `device_id`가 CMS에 등록되어 있는가?
- [ ] 방화벽에서 CMS URL 접속이 허용되는가?
- [ ] 로그에 `ffmpeg found`가 출력되는가?
- [ ] `--sample` 모드에서 영상이 나오는가?

---

## 9. 버전 정보

- 앱 버전: **0.0.1**
- 실행 파일명: `OnePlayerWin.exe`
- 정책 문서: Android OnePlayer 0.4.0 정책 계승
