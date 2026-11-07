# System lease execution

System leases resolve paths against their authorized canonical `cwd`. They never
change the host process directory and never take the file-runLua cwd mutex.
Directory object identity and package-root validation still run before each eval.
One lease remains serialized by its existing VM/session ownership.

系统租约按已授权的规范 `cwd` 解析路径，不改变宿主进程工作目录，也不获取文件型
runLua 的目录互斥锁。每次执行仍检查目录对象身份及包根；同一租约仍由既有虚拟机
及会话所有权串行执行。

## Supported boundaries / 支持边界

- Filesystem and managed IO paths are resolved before invoking host file APIs.
  系统文件及受管 IO 路径在调用宿主文件接口前完成解析。
- `loadfile`, `dofile` and Lua script `require` use Unicode-aware host reads.
  They require explicit filenames; stdin script loading is unavailable.
  脚本加载通过支持 Unicode 的宿主读取完成，必须提供文件名，不支持从标准输入加载脚本。
- Module path and cpath templates are absolute, including later template additions.
  模块路径模板始终绝对化，后续追加的模板同样受此规则约束。
- Child executables are resolved absolutely and receive their own cwd. The parent
  process directory is irrelevant to exec, popen and process-session creation.
  子进程使用绝对程序路径及自身工作目录，启动不依赖父进程当时的工作目录。
- Managed Python/Node keep their existing package-bound snapshot and child-directory
  rules. This change does not weaken package or workspace authorization.
  受管 Python/Node 保持既有包快照及子进程目录规则，不削弱包及工作区授权。
- Raw FFI, `os.execute`, `os.exit`, `os.setlocale` and `os.tmpname` are unavailable.
  Use managed processes, managed IO and host lifecycle APIs.
  不开放原始 FFI 及上述进程全局接口，应使用受管进程、IO 与宿主生命周期接口。

## Native extensions / 原生扩展

System packages are trusted code, not a native-code sandbox. Native modules must
not call chdir or interpret relative file arguments against the ambient process
directory. Hosts must audit declared extensions and pass absolute paths or isolate
incompatible extensions in owned subprocesses. Loader wrappers cannot intercept C
code's internal filesystem calls. Native-library filename encoding remains subject
to the native loader; Unicode script loading is covered independently.

系统包属于可信代码，并非原生代码沙箱。原生模块不得调用 chdir，也不得依赖进程
当前目录解析文件参数。宿主必须审计声明的扩展，传入绝对路径，或将不兼容扩展放入
受管子进程。Lua 包装不能拦截 C 代码内部文件访问；原生动态库文件名编码受原生加载器
约束，Unicode 脚本加载单独验证。

## Deadline / 截止预算

`vulcan.runtime.remaining_timeout_ms()` returns the current System eval budget;
expired work is rejected before the next managed host entry with
`Evaluation deadline exceeded.`. Blocking host calls use the smaller of their own
timeout and that budget: `process.exec`, `io.popen`, `python.invoke`, `node.invoke`
and `process.session` reads. `process.session` close shortens its graceful wait to the
budget and, once it has passed, kills the process tree without waiting. A managed
environment build started during the eval (uv or pnpm, including lifecycle scripts
and build hooks) is killed with its process tree at the deadline, and waiting for
another builder's locks stops there. Persistent process sessions otherwise remain
explicitly owned, long-lived resources. Native calls are not forcibly interruptible:
lease ownership must remain held until they actually return. The same deadline
applies to plain runLua, runtime leases and embedded calls.

上述接口返回当前系统执行的剩余预算；到期时以 `Evaluation deadline exceeded.` 拒绝
后续受管入口。阻塞宿主调用使用自身上限与剩余预算中较小者：`process.exec`、
`io.popen`、`python.invoke`、`node.invoke` 以及 `process.session` 读取。
`process.session` 关闭会把优雅等待缩短到剩余预算，预算耗尽后不再等待、直接终止
进程树。执行期间触发的受管环境构建（uv 或 pnpm，包括生命周期脚本与构建钩子）在
截止时间连同进程树一起终止，等待其他构建方持有的锁也到此为止。除此之外，持久进程
会话仍是显式管理的长期资源。不可中断的原生调用必须在实际返回前继续占有租约。
普通 runLua、运行时租约和嵌入式调用适用同一截止时间。

## Evidence / 验证

`system_runtime_lease_host_wait_does_not_block_another_lease` proves a fast lease
finishes before the slow host callback is released. Restoring the old cwd lock
makes both concurrency regressions fail by timeout. The second regression changes
the actual process cwd while holding its ordinary lock, and checks Unicode/space
paths, scripts, exec and popen. The CI workflow executes these on native Windows,
Linux and macOS runners; CI results, not this document, determine platform status.

首个测试证明慢宿主回调放行前快租约已经完成；恢复旧锁后两项并发回归均超时失败。
第二项在持有普通执行目录锁时改变真实进程目录，检查中文及空格路径、脚本、同步
子进程与 popen。持续集成在三个原生系统执行，平台状态以实际结果为准。
