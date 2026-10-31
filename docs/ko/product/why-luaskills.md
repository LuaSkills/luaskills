# 왜 LuaSkills인가

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[한국어 문서 입구](../index.md)

LuaSkills가 해결하는 문제는 단순히 Lua를 실행하는 방법이 아닙니다.
제품이 script, tool, database, AI Agent workflow, 사용자 설치 기능을 장기적으로 운영하려면 안정적인 runtime 경계가 필요합니다.

## 제품 문제

많은 호스트 제품은 결국 다음 기능을 필요로 합니다:

- AI Agent용 로컬 tool.
- IDE, 데스크톱 앱, 개발자 tool의 workflow.
- 검색, memory, database 기반 skill.
- 제품에 포함되는 first-party skill.
- project 또는 user level에서 설치되는 skill.

어려운 점은 Lua 실행 자체가 아니라 제품 경계에 맞게 skill을 관리하는 것입니다.

## LuaSkills가 제공하는 것

LuaSkills는 관습 모음이 아니라 runtime contract를 제공합니다.

- skill package loading.
- entry discovery 및 invocation.
- strict help tree.
- runtime context injection.
- dependency path injection.
- SQLite provider routing.
- system, project, user root layering.
- Rust, C ABI, public `_json` FFI integration.

## 능력 분류

| 분류 | 가능해지는 것 |
| --- | --- |
| Runtime Core | skill 로드, root reload, entry list, skill call. |
| Skill Authoring | `vulcan.*` API와 구조화된 help로 skill 작성. |
| Product Control | 권한, budget, UI, authority를 호스트가 제어. |
| Data-aware Skills | SQLite를 runtime, host provider, space controller로 연결. |
| Multi-language Hosts | Rust, C ABI, TypeScript, Python, Go에서 같은 skill model 사용. |

## 중요한 예제

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): 실제 product-grade skill 참고 예제.
- [demo-skill](https://github.com/LuaSkills/demo-skill): 최소 skill repository template.

## 결론

LuaSkills는 Lua package를 product-grade skill로 바꾸는 runtime layer입니다.
skill을 이식 가능하게 만들고, 호스트에 제어권을 남기며, 다국어 통합에 공통 경로를 제공합니다.
