//! Playback timing JSONL logger.
//!
//! The regular tracing log answers "what happened". This file answers
//! "where did the transition spend time?" for low-spec signage devices.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

use anyhow::Result;
use chrono::Local;
use serde_json::{Map, Number, Value};

static TIMING_TX: OnceLock<Sender<String>> = OnceLock::new();

/// Typed values for extra timing fields.
pub enum TimingValue {
    String(String),
    I64(i64),
    U64(u64),
    U128(u128),
    Bool(bool),
}

impl From<&str> for TimingValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<String> for TimingValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<i64> for TimingValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for TimingValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<u128> for TimingValue {
    fn from(value: u128) -> Self {
        Self::U128(value)
    }
}

impl From<usize> for TimingValue {
    fn from(value: usize) -> Self {
        Self::U64(value as u64)
    }
}

impl From<bool> for TimingValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl TimingValue {
    fn into_json(self) -> Value {
        match self {
            Self::String(value) => Value::String(value),
            Self::I64(value) => Value::Number(Number::from(value)),
            Self::U64(value) => Value::Number(Number::from(value)),
            Self::U128(value) => {
                let capped = value.min(u64::MAX as u128) as u64;
                Value::Number(Number::from(capped))
            }
            Self::Bool(value) => Value::Bool(value),
        }
    }
}

/// Initialize `%LOCALAPPDATA%/OnePlayer/logs/playback_timing-YYYY-MM-DD.log`.
///
/// Calling this more than once is harmless; only the first sender is used.
pub fn init(logs_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(logs_dir)?;
    let file_name = format!("playback_timing-{}.log", Local::now().format("%Y-%m-%d"));
    let path = logs_dir.join(file_name);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let (tx, rx) = mpsc::channel::<String>();
    if TIMING_TX.set(tx).is_ok() {
        std::thread::Builder::new()
            .name("playback-timing-log".into())
            .spawn(move || write_loop(file, rx))
            .expect("spawn playback timing logger");
    }
    Ok(())
}

fn write_loop(file: File, rx: mpsc::Receiver<String>) {
    let mut writer = BufWriter::new(file);
    while let Ok(line) = rx.recv() {
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }
}

/// Record one JSON line. If the logger is not initialized, this is a no-op.
pub fn record(
    level: &str,
    step: impl ToString,
    event: &str,
    scene_id: Option<&str>,
    target_time_millis: Option<i64>,
    now_millis: Option<i64>,
    fields: Vec<(&str, TimingValue)>,
) {
    let Some(tx) = TIMING_TX.get() else {
        return;
    };

    let mut map = Map::new();
    map.insert(
        "ts".into(),
        Value::String(Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()),
    );
    map.insert("level".into(), Value::String(level.to_string()));
    map.insert("step".into(), Value::String(step.to_string()));
    map.insert("event".into(), Value::String(event.to_string()));
    if let Some(scene_id) = scene_id {
        map.insert("scene_id".into(), Value::String(scene_id.to_string()));
    }
    if let Some(target) = target_time_millis {
        map.insert(
            "target_time_millis".into(),
            Value::Number(Number::from(target)),
        );
    }
    if let Some(now) = now_millis {
        map.insert("now_millis".into(), Value::Number(Number::from(now)));
        if let Some(target) = target_time_millis {
            map.insert(
                "time_to_target_ms".into(),
                Value::Number(Number::from(target - now)),
            );
        }
    }
    for (key, value) in fields {
        map.insert(key.to_string(), value.into_json());
    }

    if let Ok(line) = serde_json::to_string(&Value::Object(map)) {
        let _ = tx.send(line);
    }
}
