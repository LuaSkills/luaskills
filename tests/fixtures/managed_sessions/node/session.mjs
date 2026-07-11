/**
 * Long-running JSON-lines Node sidecar used by System Plugin integration tests.
 * System Plugin 集成测试使用的长期 JSON 行 Node sidecar。
 */

// Standard-library modules used for process, file, and line-protocol operations.
// 用于进程、文件与行协议操作的标准库模块。
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { createInterface } from "node:readline";
import process from "node:process";

// Package-relative marker imported from the sibling fixture module.
// 从同级夹具模块导入的包相对标记。
import { FIXTURE_MARKER } from "./helper.mjs";
// Bare dependency resolved through the exact environment-local snapshot ancestry.
// 通过精确环境内快照祖先链解析的裸依赖。
import { DEPENDENCY_MARKER } from "managed-session-dependency";

/**
 * Emit one compact JSON protocol record and flush it through console.log.
 * 通过 console.log 输出一条紧凑 JSON 协议记录并刷新。
 *
 * @param {Record<string, unknown>} payload JSON-compatible record fields.
 * payload 参数：与 JSON 兼容的记录字段。
 * @returns {void} No return value.
 * 返回值：无返回值。
 */
function emit(payload) {
  console.log(JSON.stringify(payload));
}

// Descendant intentionally survives a natural root exit so tree cleanup remains observable.
// 后代进程会有意在根进程自然退出后继续存活，使进程树清理可被观察。
const descendant = spawn(process.execPath, ["-e", "setTimeout(() => {}, 120000)"], {
  // A separate Windows console group prevents root-console teardown from ending the descendant;
  // the inherited Job Object still guarantees complete tree cleanup.
  // 独立 Windows 控制台组防止根控制台退出连带结束后代；继承的 Job Object 仍保证完整进程树清理。
  detached: process.platform === "win32",
  stdio: "ignore",
});
// The root must be able to exit independently so Job/process-group cleanup can target the survivor.
// 根进程必须能够独立退出，才能让 Job/进程组清理命中仍存活的后代。
descendant.unref();
// Initial record identifies both processes, cwd authorization, and relative import success.
// 初始记录标识两个进程、cwd 授权结果与相对导入成功状态。
const started = {
  event: "started",
  root_pid: process.pid,
  child_pid: descendant.pid,
  cwd: process.cwd(),
  marker: FIXTURE_MARKER,
  dependency_marker: DEPENDENCY_MARKER,
  // Environment evidence proves host credentials are absent while controlled values remain.
  // 环境证据用于证明宿主凭据不可见且受控值仍然存在。
  host_secret_visible: Object.prototype.hasOwnProperty.call(
    process.env,
    "LUASKILLS_TEST_HOST_SECRET",
  ),
  managed_context_present: Object.prototype.hasOwnProperty.call(
    process.env,
    "LUASKILLS_MANAGED_CONTEXT_JSON",
  ),
  path: process.env.PATH ?? "",
};
// Optional first argument receives the startup record for rollback-cleanup assertions.
// 可选首参数会接收启动记录，供回滚清理断言使用。
const pidFile = process.argv[2];
if (pidFile !== undefined) {
  writeFileSync(pidFile, JSON.stringify(started), { encoding: "utf8" });
}
emit(started);
console.error("stderr-started-node");

// Session-local counter proves state continuity and isolation.
// 会话本地计数器用于证明状态延续与隔离。
let counter = 0;
// Line reader keeps the sidecar attached to stdin until an exit command or process teardown.
// 行读取器使 sidecar 保持连接 stdin，直到收到退出命令或进程被清理。
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const rawLine of lines) {
  // Parsed command for the current protocol iteration.
  // 当前协议迭代解析得到的命令。
  const command = JSON.parse(rawLine);
  // Stable action discriminator selected by the test.
  // 测试选择的稳定动作判别字段。
  const action = command.action;
  if (action === "echo") {
    counter += 1;
    emit({ event: "echo", value: command.value, counter, marker: FIXTURE_MARKER });
  } else if (action === "spam") {
    // Multiple flushed records exercise bounded buffering and event coalescing.
    // 多条刷新记录用于验证有界缓冲与事件合并。
    for (let index = 0; index < 64; index += 1) {
      emit({ event: "spam", index, payload: "x".repeat(64) });
    }
    emit({ event: "spam_end", counter });
  } else if (action === "exit") {
    emit({ event: "exit", counter });
    break;
  } else {
    throw new Error(`unsupported action: ${String(action)}`);
  }
}
// Explicitly release the pipe watcher after the exit command so only the detached descendant lives.
// 收到退出命令后显式释放管道监听，使仅有已分离后代继续存活。
lines.close();
process.stdin.unref();
