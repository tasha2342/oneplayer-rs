//! Windows 절전/화면꺼짐 방지.
//!
//! DID는 24시간 상시 표출 장비이므로 OS의 절전과 디스플레이 꺼짐을 막는다.

/// 시스템 절전과 디스플레이 꺼짐을 방지한다 (앱 시작 시 1회 호출).
///
/// `ES_CONTINUOUS`와 함께 호출하면 프로세스가 살아있는 동안 유지되며,
/// 프로세스 종료 시 OS가 자동으로 원복하므로 해제 처리가 필요 없다.
#[cfg(windows)]
pub fn prevent_sleep() {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };
    unsafe {
        let _ = SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
    }
}
