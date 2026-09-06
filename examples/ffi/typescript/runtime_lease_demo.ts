/**
Minimal TypeScript host demo for one persistent runtime lease.
演示单个持久运行时租约的最小 TypeScript 宿主示例。
 */

import koffi from "koffi";

import {
  JsonFfiClient,
  SKILL_AUTHORITY_SYSTEM,
  StandardFixtureRuntimeClient,
  resolveLibraryPath,
  resolveStandardFixtureRuntimeRoot,
} from "./json_runtime.ts";

/**
Stable session id reused by the demo lease lifecycle.
演示租约生命周期复用的稳定会话标识。
 */
const RUNTIME_SESSION_SID = "typescript-runtime-lease-demo";

/**
Run one persistent runtime-lease smoke flow through the JSON FFI surface.
通过 JSON FFI 接口执行一条持久运行时租约烟测链路。
 */
function main(): void {
  const library = koffi.load(resolveLibraryPath());
  const client = new JsonFfiClient(library);
  const runtime = new StandardFixtureRuntimeClient(
    client,
    resolveStandardFixtureRuntimeRoot(),
  );

  const engineId = runtime.createEngine("utf-8", true);
  console.log("Engine created:", String(engineId));

  try {
    runtime.loadRoot(engineId);
    console.log("Loaded roots from:", `${runtime.runtimeRoot}/skills`);

    const system = runtime.systemClient(engineId, SKILL_AUTHORITY_SYSTEM);
    console.log("Visible entry count:", system.listEntries().length);
    console.log(
      "Visible skill ownership:",
      system.skillNameForTool("demo-standard-ffi-skill-ping"),
    );

    const sessions = system.runtimeLeases();
    console.log(
      "Uses dedicated system runtime-lease endpoints:",
      sessions.usesSystemRuntimeLeaseEndpoints(),
    );
    const session = sessions.createHandle(RUNTIME_SESSION_SID, 600, true);
    const identity = session.identityPayload();
    const leaseId = identity.lease_id;
    console.log("Lease created:", leaseId);
    console.log("Lease handle count:", sessions.listHandles(RUNTIME_SESSION_SID).length);

    const opened = session.eval(
      `
local info = vulcan.os.info()
if not proc then
  local spec
  if info.os == "windows" then
    spec = {
      program = "cmd",
      args = { "/V:ON", "/C", "set /P line=&echo session:!line!" },
      encoding = "utf-8",
    }
  else
    spec = {
      program = "sh",
      args = { "-c", "read line; echo session:$line" },
      encoding = "utf-8",
    }
  end
  proc = vulcan.process.session.open(spec)
end
counter = (counter or 0) + 1
proc:write((args.input or "runtime-lease-demo") .. "\\n")
return {
  opened = true,
  counter = counter,
  input = args.input,
}
`,
      {
        input: "runtime-lease-demo",
      },
    );
    console.log("Open eval result:", opened.result);

    const readOutput = session.eval(
      `
counter = (counter or 0) + 1
local output = proc:read({ timeout_ms = 2000, max_bytes = 8192 })
return {
  counter = counter,
  stdout = output.stdout,
  stderr = output.stderr,
  timed_out = output.timed_out,
}
`,
    );
    console.log("Read eval result:", readOutput.result);

    const currentStatus = session.status();
    console.log("Lease status result:", currentStatus);

    const closedProcess = session.eval(
      `
counter = (counter or 0) + 1
local status = proc:close({ timeout_ms = 3000 })
proc = nil
return {
  counter = counter,
  exited = status.exited,
  success = status.success,
}
`,
    );
    console.log("Close process eval result:", closedProcess.result);

    const closedLease = session.close();
    console.log("Lease close result:", closedLease);

    const postClose = sessions.callRaw("eval", {
      lease_id: leaseId,
      sid: identity.sid,
      generation: identity.generation,
      timeout_ms: 60_000,
      args: {},
      code: "return 1",
    });
    console.log("Post-close eval result:", postClose);
  } finally {
    runtime.freeEngine(engineId);
    console.log("Engine freed");
  }
}

main();
