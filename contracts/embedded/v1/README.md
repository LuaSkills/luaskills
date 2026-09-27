# 嵌入式运行时离线契约

`contract.json` 从实际 Rust 请求、响应及其嵌套类型生成，`contract.sha256` 校验其完整 UTF-8 字节。这里是当前开发源码的生成结果，不表示同版本已发布的动态库已经支持新增接口。

生成与只读检查：

```powershell
rtk proxy cargo +1.94.0 run --locked --features contract-generation --bin generate_embedded_contract -j 4
rtk proxy cargo +1.94.0 run --locked --features contract-generation --bin generate_embedded_contract -j 4 -- --check
```

生成器的 `contract-generation` 功能默认关闭，普通 DLL、静态库及宿主构建不需要加载 Schema 生成器。Schemars 版本由 `Cargo.toml` 精确固定并纳入 `Cargo.lock`；依赖锁、生成入口源码摘要及核心包版本保存在产物中。生成源码摘要归一化 CRLF；产物经 `.gitattributes` 固定 LF，不能按平台重新转换后复用旧摘要。

`request` 使用实际 Rust 反序列化类型；`error_response`、`root_responses`、`runtime_responses` 使用实际序列化类型。每项均是独立 Draft 2020-12 根 Schema，局部 `$ref` 只能在该项自身的 `$defs` 内解析，不能拿整个契约容器替代根 Schema。运行时响应类型来自 `src/ffi_embedded/responses.rs`，原生分发通过同一类型别名约束真实返回值；宿主请求批次直接使用代理实际序列化的 `HostRequest` 类型。`runtime` 根命令按其 `operation.type` 选择 `runtime_responses`，其余根命令按 `command.type` 选择 `root_responses`。

Schema 描述线帧的字段、类型和省略规则，不替代核心语义校验：协议支持版本、正数及关联预算、状态转换、权限、期限、Schema 合法性和宿主完成布尔值与形状一致性仍由真实核心校验。重复 JSON 键也必须由实际解析器拒绝，不能先解码成对象后声称已检查重复键。输出中任意 JSON `value` 的显式空值与字段缺失必须保留区分；观察等待到期返回当前操作快照，不返回空值。

SDK 同步时复制完整契约及摘要到自己的发布包，再从这份离线副本生成；不能依赖开发者相邻源码目录。生成器遇到未支持的 Schema 形状应报错，不把未知类型降为任意对象。身份字符串保持原样，C ABI 的 `u64` 身份保留全部 64 位；其他有界 JSON 整数也不得经过语言浮点数静默舍入。

`json-vectors.json` 是核心拥有的跨语言语料输入，生成器将其完整嵌入 `contract.json` 的 `json_vectors`，因此 SDK 包内契约摘要同时覆盖这些向量。SDK 不需要额外的相邻核心路径或独立手工副本。向量版本与线协议版本分别管理；更新语料后必须重新生成契约和三个 SDK 产物，并运行各语言的共享向量测试。

当前语料覆盖精确 64 位整数、浮点意图及负零的位表示、空值／布尔／空集合、Unicode、对象键、重复字段、畸形字节和信封。整数字面量范围是有符号 64 位与无符号 64 位的并集；超范围整数直接拒绝，不转为浮点。显式小数或指数允许所有有限双精度值；`-0` 保留浮点负零。实际参数中的 Lua 表示、预算和业务约束继续由运行时负责，线表示无损不等于 Lua 数值运算具有任意精度。

核心先以有深度限制的 serde_json 访问器检查全部嵌套键，再检查整数词元范围，最后执行原类型化反序列化及语义验证。校验发生在任何命令分发之前，不把已解析映射当成重复字段证据。SDK 解析失败仍须释放实际返回缓冲；宿主完成值编码失败仍由各自回调泵的完成恢复规则处理，不能将编码失败解释成副作用未发生。

执行 `rtk proxy cargo +1.94.0 test --locked --features contract-generation --lib embedded_ -j 4 -- --test-threads=1`，将检查产物字节、摘要、离线引用、命令覆盖、空值和严格字段，并把真实 C 入口和 Lua 执行结果对照对应 Schema 校验。五平台工作流已加入相同检查；本地执行通过不代表远端矩阵已通过。

上游依据：[Schemars 的 Serde 属性支持](https://graham.cool/schemars/deriving/attributes/)和[序列化／反序列化生成方向](https://docs.rs/schemars/1.2.2/schemars/generate/struct.SchemaSettings.html)。生成类型、Serde 属性、返回结构、错误枚举、依赖锁或生成逻辑变化时，必须重新生成并校验，再更新各 SDK 副本。
