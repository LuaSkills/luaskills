# LuaSkills 문서

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](../ja/index.md) | [한국어](index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[한국어 README](../../README.ko.md) | [왜 LuaSkills인가](product/why-luaskills.md)

문서는 작업별로 스킬 개발, 호스트 통합, SDK의 세 가지로 나뉩니다. 기술 문서는 영어로, 호스트와 FFI 심화 참조는 중국어로 관리합니다.

## 시작하기

| 하려는 일 | 시작 문서 |
| --- | --- |
| LuaSkills 평가 | [왜 LuaSkills인가](product/why-luaskills.md) |
| Lua 스킬 작성 | [Skill development manual](../development/skill-development.md) |
| System Plugin 또는 관리형 Python/Node 흐름 작성 | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| Rust 호스트에 내장 | [Runtime architecture](../integration/runtime-model.md) |
| C ABI 또는 JSON FFI로 내장 | [FFI and integration overview](../integration/ffi-overview.md) |
| TypeScript, Python, Go 사용 | [SDK overview](../sdk/index.md) |

## 스킬 개발

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill): 최소 스킬 템플릿.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): 프로덕션 수준 예제.

### 스킬 이름 규칙

`skill_id`와 모든 `entry.name`은 `^[a-z]([a-z0-9-]*[a-z0-9])?$`와 일치해야 합니다.
물리적 스킬 디렉터리 이름이 유일한 `skill_id`이며, `skill.yaml`에는 `skill_id` 필드를 선언하면 안 됩니다.
canonical entry는 `{skill_id}-{entry_name}`으로 노출되며 충돌 시 안정적인 `-N` 접미사가 붙을 수 있습니다.
GitHub로 관리되는 스킬은 저장소에서 파생되었거나 명시된 `skill_id`, 릴리스 zip 접두사, checksum 접두사, zip 최상위 디렉터리, 설치 디렉터리가 모두 같아야 합니다.
릴리스 자산은 `{skill_id}-v{version}-skill.zip`과 `{skill_id}-v{version}-checksums.txt`를 사용하며, zip에는 `{skill_id}/skill.yaml`이 포함되어야 합니다.

## 호스트 통합

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [중국어 상세 참조](../zh-CN/index.md#宿主对接)

## SDK

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## 로컬 예제

- [Rust 데모](../../examples/demo-rust/README.md)
- [C FFI 데모](../../examples/ffi/c/README.md)
- [TypeScript FFI 데모](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
