/**
 * Managed Node invoke handler used by System Plugin integration tests.
 * System Plugin 集成测试使用的受管 Node invoke 处理器。
 */

// Package-scoped dependency resolved from the exact managed node_modules tree.
// 从精确受管 node_modules 树解析的包级依赖。
import { DEPENDENCY_MARKER } from "managed-session-dependency";
// File-URL conversion used to expose exact snapshot-source evidence.
// 用于暴露精确快照源码证据的文件 URL 转换。
import { fileURLToPath } from "node:url";
// Relative ESM dependency resolved from the immutable snapshot module URL.
// 从不可变快照模块 URL 解析的相对 ESM 依赖。
import { FIXTURE_MARKER } from "./helper.mjs";
// Process environment access used only for boundary evidence.
// 仅用于边界证据的进程环境访问。
import process from "node:process";

// Module-local invocation count used to prove worker module-cache isolation.
// 用于证明 Worker 模块缓存隔离的模块内调用计数。
let invocationCount = 0;

/**
 * Return dependency, module-state, argument, and trusted package evidence.
 * 返回依赖、模块状态、参数与可信包证据。
 *
 * @param {Record<string, unknown>} args JSON-compatible invocation arguments.
 * args 参数：与 JSON 兼容的调用参数。
 * @param {Record<string, unknown>} ctx Engine-controlled package and System lease context.
 * ctx 参数：由引擎控制的包与 System 租约上下文。
 * @returns {Promise<Record<string, unknown>>} JSON-compatible integration evidence.
 * 返回值：与 JSON 兼容的异步集成证据。
 */
export default async function main(args, ctx) {
  // InvocationCount intentionally persists only inside this loaded package module.
  // InvocationCount 有意仅在当前已加载包模块内部持久存在。
  invocationCount += 1;
  // Optional oversized output proves the worker wrapper retains only its bounded tail.
  // 可选超大输出用于证明 Worker 包装器只保留有界尾部。
  if (args.spam === true) {
    console.log("x".repeat(300 * 1024));
  }
  // PackageContext is mandatory because the worker receives only trusted engine context.
  // PackageContext 是必需项，因为 Worker 只接收引擎提供的可信上下文。
  const packageContext = ctx.package;
  console.log(`node-invoke:${packageContext.id}:${invocationCount}`);
  return {
    counter: invocationCount,
    dependency_marker: DEPENDENCY_MARKER,
    relative_marker: FIXTURE_MARKER,
    host_secret_visible: Object.prototype.hasOwnProperty.call(
      process.env,
      "LUASKILLS_TEST_HOST_SECRET",
    ),
    package_id: packageContext.id,
    path: process.env.PATH ?? "",
    cwd: process.cwd(),
    source_path: fileURLToPath(import.meta.url),
    value: args.value,
  };
}
