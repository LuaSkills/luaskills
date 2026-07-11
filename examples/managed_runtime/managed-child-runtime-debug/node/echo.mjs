// Import one third-party dependency installed by the managed Node.js environment.
// 导入一个由受管 Node.js 环境安装的第三方依赖。
import isOdd from "is-odd";
// Import one named CommonJS export through ESM interop.
// 通过 ESM 互操作导入一个 CommonJS 命名导出。
import { default as isNumberNamed } from "is-number";
// Import one namespace binding from a managed dependency.
// 从受管依赖导入一个命名空间绑定。
import * as isNumberNamespace from "is-number";
// Import one local helper through a relative ESM path.
// 通过相对 ESM 路径导入一个本地辅助函数。
import { localMarker } from "./local-helper.mjs";
// Import one module only for side effects.
// 仅为了副作用导入一个模块。
import "./side-effect.mjs";

// Handle one managed Node.js smoke request from Lua.
// 处理一次来自 Lua 的受管 Node.js 冒烟请求。
export async function main(args, ctx) {
  // stdout proves the worker captures Node.js console output.
  // stdout 用于证明 worker 会捕获 Node.js console 输出。
  console.log("node stdout ready");
  // The returned object proves JSON value transport and argument passing.
  // 返回对象用于证明 JSON 值传输与参数传递正常。
  return {
    runtime: "node",
    dependency: isOdd(3) ? "is-odd" : "unexpected",
    namedImport: isNumberNamed(4) ? "is-number-named" : "unexpected",
    namespaceImport: isNumberNamespace.default(5) ? "is-number-namespace" : "unexpected",
    relativeImport: localMarker(),
    sideEffectImport: globalThis.__luaskillsSideEffectMarker || "missing",
    text: args.text || "",
    number: (args.number || 0) + 2,
    ctxIsObject: ctx && typeof ctx === "object",
  };
}
