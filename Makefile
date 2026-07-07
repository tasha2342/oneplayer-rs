.PHONY: build test fmt clippy package sample

# 릴리즈 빌드 (산출물: target/release/OnePlayerWin[.exe])
build:
	cargo build --release -p oneplayer

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# dist/OnePlayerWin-v{version}.exe 형태로 복사
package:
	./scripts/package.sh

# 샘플 스케줄 데모 (Android Phase 1 동등 마일스톤)
sample:
	cargo run -p oneplayer -- --sample
