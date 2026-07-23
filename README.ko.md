# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[문서](docs/ko/index.md) | [Skill 템플릿](https://github.com/LuaSkills/demo-skill) | [CodeKit 예제](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills는 Lua로 작성된 skill을 로드, 실행, 관리하기 위한 Rust 기반 런타임입니다.
호스트 애플리케이션은 별도의 plugin 런타임을 매번 만들지 않고도 스크립트형 도구, 구조화된 help, 런타임 API, 의존성 경로, SQLite / LanceDB 연동을 추가할 수 있습니다.

한 문장으로 말하면, LuaSkills는 skill을 실행하고 호스트는 그 skill을 어떤 제품 기능으로 노출할지 결정합니다.

## 무엇인가

LuaSkills는 LuaSkills 생태계의 핵심 런타임 계층입니다.
일회성 스크립트가 아니라 제어 가능한 skill 시스템이 필요한 애플리케이션을 위해 설계되었습니다.

제공 기능:

- skill 발견, 로드, entry 목록화, 호출.
- 문서, 명령 팔레트, tool, UI로 렌더링 가능한 strict help tree.
- `vulcan.*` 및 `vulcan.runtime.*` 표준 Lua API 주입.
- Windows x64, Linux x64/ARM64, macOS x64/ARM64에서 skill과 System Plugin이 사용하는 패키지 격리형 영구 Python/Node.js 세션. Windows ARM은 명시적으로 지원하지 않습니다.
- 요청, skill 디렉터리, 리소스, 의존성 루트, 클라이언트 정보에 대한 런타임 컨텍스트 주입.
- 상태형 또는 memory skill을 위한 SQLite / LanceDB 연동.
- Rust API, 표준 C ABI, public `_json` FFI.
- TypeScript, Python, Go SDK 통합 경로.

## 무엇이 아닌가

LuaSkills는 제품 전체를 소유하지 않습니다.

아닙니다:

- MCP server 자체.
- 호스트 설정 파일 리더.
- 클라이언트 budget 계산기.
- 제품 UI 렌더러.
- 임의의 신뢰할 수 없는 Lua 코드를 위한 sandbox 경계.

권한, 인증, UI, budget, 저장 위치, 사용자에게 노출되는 방식은 호스트가 제어합니다.

## 생태계

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): 실제 제품 수준의 중요한 LuaSkills 예제입니다. 코드 탐색, AST 검사, 구조 검색, Markdown 탐색, patch workflow를 보여줍니다.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl): 구조화된 GET / POST entry와 curl 스타일 request 실행을 제공하는 HTTP request skill입니다.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file): ignore 규칙을 반영한 file list, 정확한 text read, preview-first 소규모 edit를 다루는 file operation skill입니다.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua): inline Lua code 또는 Lua file task를 제한된 방식으로 실행하는 controlled Lua execution skill입니다.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit): build, test, lint, typecheck 출력을 compact diagnostics로 정리하는 validation router skill입니다.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem): task checkpoint와 handoff context를 저장하는 project-scoped working memory skill입니다.
- [demo-skill](https://github.com/LuaSkills/demo-skill): `skill.yaml`, runtime entry, help 파일, 기본 디렉터리 구조를 배우기 위한 최소 skill 템플릿입니다.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript): TypeScript / Node.js SDK.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python): Python SDK.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go): Go SDK.

## 문서

- [한국어 문서 입구](docs/ko/index.md)
- [왜 LuaSkills인가](docs/ko/product/why-luaskills.md)
- [영어 문서 입구](docs/index.md)
- [중국어 상세 기술 문서](docs/zh-CN/index.md)

## 통합 경로

| 호스트 유형 | 권장 경로 |
| --- | --- |
| Rust | Rust crate를 직접 사용합니다. |
| C / C++ / 저수준 호스트 | 표준 C ABI를 사용합니다. |
| TypeScript / Node.js | `luaskills-sdk-typescript`를 우선 사용합니다. |
| Python | `luaskills-sdk-python`을 우선 사용합니다. |
| Go | callback 및 배포 요구에 따라 `luaskills-sdk-go` 또는 표준 C ABI를 선택합니다. |

## 빠른 시작

Rust 호스트:

```toml
[dependencies]
luaskills = "0.5"
```

개발 명령:

```bash
cargo check
cargo test --lib
```

skill 구조를 배우려면:

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Skill development manual](docs/skill-development.md)

## Skill 명명 규칙

`skill_id`와 각 `entry.name`은 `^[a-z]([a-z0-9-]*[a-z0-9])?$`와 일치해야 합니다.
물리적인 skill 디렉터리 이름만 `skill_id`의 유일한 출처이며, `skill.yaml`은 `skill_id` 필드를 선언하면 안 됩니다.
canonical entry는 `{skill_id}-{entry_name}` 형식을 사용하며, 충돌이 있으면 안정적인 `-N` 접미사가 붙을 수 있습니다.
GitHub 관리 skill에서는 저장소에서 파생되었거나 명시된 `skill_id`, release zip 접두사, checksum 접두사, zip 최상위 디렉터리, 최종 설치 디렉터리가 모두 같아야 합니다.
asset은 `{skill_id}-v{version}-skill.zip`과 `{skill_id}-v{version}-checksums.txt`를 사용하며, zip에는 `{skill_id}/skill.yaml`이 포함되어야 합니다.

## 신뢰 모델

현재 LuaSkills는 skill을 기본적으로 신뢰된 코드로 취급합니다.
임의의 신뢰할 수 없는 Lua package에 대한 sandbox 보장은 제공하지 않습니다.

호스트는 root, 설치 가능한 skill, 관리 동작, database provider mode, operation authority를 명확히 결정해야 합니다.

## 현재 버전 매트릭스

| 구성 요소 | 현재 버전 |
| --- | --- |
| LuaSkills core, FFI SDK 및 언어 SDK 계열 | `0.5.4` |
| Lua runtime packages 호환 계열 | `0.1` |
| 관리형 Python / uv | `3.14.6` / `0.11.28` |
| 관리형 Node.js / pnpm | `24.18.0` / `11.11.0` |
| 관리형 런타임 엔진 기본값 | `Worker=4 / 유휴=60초 / 세션=256 / 버퍼=1MiB/스트림 / invoke=무제한` |
| vldb-controller | `0.2.3` |
| vldb-sqlite / vldb-lancedb | `0.1.6` / `0.1.5` |

실제 기본값은 `scripts/deps/fetch_managed_runtimes.*`, `scripts/deps/fetch_deps.*`, `scripts/deps/sync_runtime_assets.*`에 정의됩니다. 호스트가 다른 정확한 지원 버전을 의도적으로 설치하고 선언하지 않는 한 `dependencies.yaml`에는 위 관리형 runtime 및 패키지 관리자 버전을 정확히 선언해야 합니다.

## License

MIT
