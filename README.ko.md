<div align="center">

# LuaSkills

**Lua 스크립트를 제품 수준의 스킬로 만드는 Rust 런타임**

Rust 코어 · LuaJIT 스킬 · Rust API, C ABI, JSON FFI, TypeScript / Python / Go SDK

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | **한국어** | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[문서](docs/ko/index.md) · [스킬 템플릿](https://github.com/LuaSkills/demo-skill) · [CodeKit 예제](https://github.com/LuaSkills/vulcan-codekit)

</div>

> 이 문서는 [영어 README](README.md)의 번역입니다. 내용이 다르면 영어판이 기준입니다.

---

## 소개

LuaSkills는 호스트 애플리케이션 안에서 Lua 스킬 패키지를 로드하고 실행하며 관리합니다. 호스트는 자체 플러그인 런타임을 만들지 않고도 스크립트 가능한 도구, 구조화된 도움말, 관리형 Python 및 Node.js 런타임, 의존성 관리, 데이터베이스를 인식하는 실행을 얻습니다.

**LuaSkills는 스킬을 실행하고, 스킬을 어떤 제품 기능으로 만들지는 호스트가 결정합니다.** 정책, 인증, 권한 확인, 예산, 저장 위치, 사용자 경험은 호스트에 남습니다.

LuaSkills는 [Vulcan Code](https://github.com/OpenVulcan/vulcan-code)의 스킬 시스템을 구동합니다. 일회성 내장 스크립트가 아니라 통제된 스킬 시스템이 필요한 AI 에이전트, IDE, 데스크톱 또는 서버 제품에 적합합니다.

## 주요 기능

### 🧩 스킬 런타임

- `ROOT`, `PROJECT`, `USER` 계층 루트에서 스킬 검색, 로드, 목록, 호출, 다시 로드
- 문서, 도구 설명, 명령 팔레트, UI 패널로 렌더링할 수 있는 엄격한 도움말 트리
- 엔트리마다 완전한 객체 입력 스키마와 정규 이름 `{skill_id}-{entry_name}`
- GitHub 릴리스나 URL에서 설치·업데이트·제거, 체크섬으로 검증된 의존성

### 🧰 표준 Lua API

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- 요청·클라이언트·예산·스킬 디렉터리 정보를 담은 `vulcan.context`, 의존성 루트를 위한 `vulcan.deps`
- `vulcan.host.*`로 호스트 등록 도구를, `vulcan.models.*`로 호스트가 관리하는 임베딩 모델과 LLM을 사용
- `vulcan.config.*`는 패키지마다 타입이 있고 검증되는 설정을 버전 관리 저장소와 변경 이벤트로 제공

### 🐍 관리형 Python과 Node.js

- `vulcan.runtime.python.*`과 `vulcan.runtime.node.*`로 버전이 지정된 Python / Node.js 핸들러 호출
- 잠긴 패키지 의존성, 제한된 출력, 확정적인 프로세스 트리 정리
- 엄격한 System Plugin 리스에서는 호출 간 지속 세션, 호스트 이벤트, 에지 트리거 웨이크 콜백 제공
- 호스트가 지정하는 읽기 전용 배포 루트와 쓰기 가능한 환경 루트

### 🔌 호스트 통합

- Rust 호스트용 crate API
- 그 밖의 언어를 위한 표준 C ABI와 공개 `_json` FFI
- 지속 Lua VM을 위한 공개 `runtime_lease`와 권한에 묶인 `system_runtime_lease`
- 구조화된 `host_result` 반환, 첫 번째 종류는 IDE 수준의 `change_set` 편집
- 동적 라이브러리, 호스트 콜백, space controller를 통한 SQLite(선택)

### 📦 SDK

- JSON FFI 기반 [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python), [Go](https://github.com/LuaSkills/luaskills-sdk-go) SDK
- SHA-256 검증을 포함한 런타임 자산 설치

## LuaSkills가 아닌 것

- 그 자체로 MCP 서버가 아닙니다. MCP 호스트 계층은 [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp)가 제공합니다
- 호스트 설정 리더, 예산 계산기, UI 렌더러가 아닙니다
- **샌드박스가 아닙니다.** 스킬은 신뢰된 코드로 취급됩니다. 어떤 루트를 켤지, 어떤 스킬을 설치할지, 어떤 관리 작업을 노출할지, 어떤 데이터베이스 모드를 허용할지, 각 작업이 어떤 권한을 가질지는 호스트가 정합니다.

## 아키텍처

```
┌──────────────────────────────┐         ┌──────────────────────────────────────┐
│  Host application            │  Rust   │  LuaSkills runtime                   │
│  policy · UI · permissions   │  C ABI  │  skill loader · help · lifecycle     │
│  budgets · model providers   │◀───────▶│  LuaJIT VMs · leases · vulcan.* APIs │
│  host tools · storage        │  JSON   │  managed Python / Node · deps        │
│                              │  FFI    │  config stores · SQLite    │
└──────────────────────────────┘         └──────────────────────────────────────┘
                                                          │
                                              skills/<skill_id>/skill.yaml
```

| 디렉터리 | 설명 |
| --- | --- |
| `src/runtime` | 엔진, Lua VM, 리스, 컨텍스트, 도움말, 결과, 관리형 런타임 |
| `src/skill` | 매니페스트, 루트, 설정, 수명 주기 관리 |
| `src/dependency`, `src/download` | 의존성 해석, 다운로드, 아카이브 추출 |
| `src/providers` | SQLite 바인딩 |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | 공개 `_json` FFI, 표준 C ABI, 임베디드 런타임 전송 |
| `include/` | FFI 호스트용 C 헤더 |
| `contracts/` | SDK와 공유하는 기계 판독 계약 |
| `examples/` | Rust, FFI, 관리형 런타임, 설정 예제 |

## 시작하기

### Rust 호스트

```toml
[dependencies]
luaskills = "0.7.4"
```

[`examples/demo-rust`](examples/demo-rust/README.md)에서 `call_skill`과 `vulcan.host.*` 도구 브리지를 볼 수 있습니다.

### 다른 언어

| 호스트 | 방법 |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | [Releases](https://github.com/LuaSkills/luaskills/releases)에서 `luaskills-ffi-sdk-{platform}.tar.gz`를 받아 표준 C ABI 사용 |

[SDK overview](docs/sdk/index.md)와 [FFI overview](docs/integration/ffi-overview.md)를 참고하세요.

### 스킬 작성

[demo-skill](https://github.com/LuaSkills/demo-skill)에서 시작한 뒤 [Skill development manual](docs/development/skill-development.md)을 읽으세요. 실제 런타임 구성으로 스킬 하나를 디버그하려면:

```bash
cargo run --features debug-sqlite --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug`는 스킬을 `runtime_root/skills/<skill_id>`로 복사하고 일반 `load_from_roots -> call_skill` 경로로 실행하므로, 의존성 루트·상태·데이터베이스·컨텍스트가 실제 호스트 실행과 같습니다. 저장소 내부 개발 도구이며 SDK나 FFI API를 추가하지 않습니다.

## 문서

| 분류 | 문서 |
| --- | --- |
| 스킬 개발 | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| 호스트 통합 | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDK | [SDK overview](docs/sdk/index.md) |
| 전체 | [한국어 문서](docs/ko/index.md) · [Documentation hub](docs/index.md) |

## 생태계

| 저장소 | 설명 |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | 코드 탐색, AST 검사, 구조 검색, Markdown 탐색, 안전한 패치 |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | 구조화된 HTTP GET / POST 요청 |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | 무시 규칙을 반영한 목록, 정확한 읽기, 미리보기 우선의 작은 편집 |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | 제한된 인라인 또는 파일 기반 Lua 실행 |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | 빌드, 테스트, lint, 타입 검사 출력을 간결한 진단으로 변환 |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | 체크포인트와 인수인계를 위한 프로젝트 단위 작업 메모리 |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | 최소 스킬 저장소 템플릿 |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Lua 런타임 패키지와 네이티브 의존성 번들 |

## 버전

| 구성 요소 | 버전 |
| --- | --- |
| LuaSkills 코어와 FFI SDK | `0.7.4` |
| TypeScript, Python, Go SDK | 독립적인 버전 라인. [SDK overview](docs/sdk/index.md#versions) 참고 |
| Lua 런타임 패키지 | 호환 `0.1` 시리즈 |
| 관리형 Python / uv | `3.14.6` / `0.11.28` |
| 관리형 Node.js / pnpm | `24.18.0` / `11.11.0` |
| 관리형 런타임 기본값 | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |

스크립트 기본값은 `scripts/deps/`에 있습니다. 릴리스는 [릴리스 절차](scripts/release/README.md)에 따라 런타임 패키지, 5개 플랫폼 코어 릴리스, crate, SDK 순으로 진행합니다.

## 기여하기

Issue와 Pull Request를 환영합니다.

1. 저장소를 Fork하고 `main`에서 브랜치를 만듭니다.
2. 로컬에서 검증합니다:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. 모든 커밋에 `git commit -s`로 서명합니다. DCO는 [CONTRIBUTING.md](CONTRIBUTING.md)를 참고하세요.
4. PR마다 한 가지에 집중하고, 무엇을 왜 바꿨는지와 어떻게 검증했는지 설명해 주세요.

## 라이선스

[MIT](LICENSE). 서드파티 고지는 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)에 있습니다.
