//! SNTP(UDP) 클라이언트 구현.
//!
//! Android OnePlayer의 `SntpClient.kt`와 동일한 최소 구현이다:
//! 48바이트 NTP 패킷을 UDP 123 포트로 보내고 transmit timestamp를 읽는다.

use async_trait::async_trait;

use super::{NtpClient, NtpResult};

/// NTP 표준 포트.
const NTP_PORT: u16 = 123;
/// NTP 패킷 크기 (헤더만, 확장 없음).
const NTP_PACKET_SIZE: usize = 48;
/// transmit timestamp 필드의 바이트 오프셋.
const TRANSMIT_TIME_OFFSET: usize = 40;
/// NTP epoch(1900-01-01)와 Unix epoch(1970-01-01)의 차이 (초).
const NTP_TO_UNIX_SECONDS: i64 = 2_208_988_800;

/// UDP 기반 SNTP 클라이언트.
pub struct SntpClient {
    /// 송수신 타임아웃 (ms).
    timeout_ms: u64,
}

impl Default for SntpClient {
    /// 기본 타임아웃 3초로 생성한다.
    fn default() -> Self {
        Self { timeout_ms: 3_000 }
    }
}

impl SntpClient {
    /// 타임아웃을 지정해 생성한다.
    pub fn new(timeout_ms: u64) -> Self {
        Self { timeout_ms }
    }

    /// NTP 응답 버퍼에서 transmit timestamp를 epoch millis로 변환한다.
    ///
    /// NTP 타임스탬프는 [초(32bit) | 소수부(32bit)] 형식이며,
    /// 소수부는 2^32 분율이므로 `fraction * 1000 / 2^32`로 밀리초를 얻는다.
    fn read_transmit_time(buffer: &[u8]) -> i64 {
        let seconds = read_unsigned_int(buffer, TRANSMIT_TIME_OFFSET);
        let fraction = read_unsigned_int(buffer, TRANSMIT_TIME_OFFSET + 4);
        let millis = (fraction * 1_000) / 0x1_0000_0000;
        ((seconds - NTP_TO_UNIX_SECONDS) * 1_000) + millis
    }
}

/// 빅엔디언 32bit unsigned 정수를 읽는다.
fn read_unsigned_int(buffer: &[u8], offset: usize) -> i64 {
    ((buffer[offset] as i64 & 0xff) << 24)
        | ((buffer[offset + 1] as i64 & 0xff) << 16)
        | ((buffer[offset + 2] as i64 & 0xff) << 8)
        | (buffer[offset + 3] as i64 & 0xff)
}

#[async_trait]
impl NtpClient for SntpClient {
    /// SNTP 요청을 보내고 서버 시각과 왕복 시간을 반환한다.
    ///
    /// UDP 소켓은 blocking I/O이므로 `spawn_blocking`으로 tokio 워커를
    /// 막지 않게 처리한다.
    async fn request_time(&self, server: &str) -> anyhow::Result<NtpResult> {
        let server = server.to_string();
        let timeout_ms = self.timeout_ms;
        tokio::task::spawn_blocking(move || {
            use std::net::UdpSocket;
            use std::time::{Duration, Instant};

            let socket = UdpSocket::bind("0.0.0.0:0")?;
            socket.set_read_timeout(Some(Duration::from_millis(timeout_ms)))?;
            socket.set_write_timeout(Some(Duration::from_millis(timeout_ms)))?;

            // 첫 바이트: LI=0, VN=4, Mode=3(client).
            let mut buffer = [0u8; NTP_PACKET_SIZE];
            buffer[0] = 0b00_100_011;

            let started = Instant::now();
            socket.send_to(&buffer, (server.as_str(), NTP_PORT))?;
            let (len, _) = socket.recv_from(&mut buffer)?;
            let round_trip = started.elapsed();

            if len < NTP_PACKET_SIZE {
                anyhow::bail!("short NTP response: {len} bytes");
            }
            Ok(NtpResult {
                epoch_millis: Self::read_transmit_time(&buffer),
                round_trip_millis: round_trip.as_millis() as i64,
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// transmit timestamp 파싱이 epoch millis로 올바르게 변환되는지 확인한다.
    #[test]
    fn read_transmit_time_parses_epoch() {
        let mut buf = [0u8; 48];
        let unix_seconds = 1_700_000_000i64 + NTP_TO_UNIX_SECONDS;
        buf[40] = ((unix_seconds >> 24) & 0xff) as u8;
        buf[41] = ((unix_seconds >> 16) & 0xff) as u8;
        buf[42] = ((unix_seconds >> 8) & 0xff) as u8;
        buf[43] = (unix_seconds & 0xff) as u8;
        assert_eq!(SntpClient::read_transmit_time(&buf), 1_700_000_000_000);
    }
}
