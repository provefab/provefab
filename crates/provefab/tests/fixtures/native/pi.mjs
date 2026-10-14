import assert from "node:assert/strict";
import { readFileSync, writeFileSync, unlinkSync, existsSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

// Actual installed factories, actual adapter-generated policy, actual built guard.
const factory = await import(pathToFileURL(process.env.PI_TEST_MODULE).href);
const module = new vm.SourceTextModule(stripTypeScriptTypes(readFileSync(process.env.NATIVE_EXTENSION, "utf8")));
await module.link(async specifier => {
  const exports = specifier === "@earendil-works/pi-coding-agent" ? factory : await import(specifier);
  return new vm.SyntheticModule(Object.keys(exports), function () {
    for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
  });
});
await module.evaluate();
const tools = new Map(), handlers = new Map();
module.namespace.default({ registerTool: tool => tools.set(tool.name, tool), on: (name, handler) => handlers.set(name, handler) });
assert.deepEqual([...tools.keys()], ["provefab_bash"]);
const context = { cwd: process.env.PROVEFAB_WORKTREE };
const result = await tools.get("provefab_bash").execute("native", { command: process.env.NATIVE_COMMAND, timeout: 40 }, undefined, undefined, context);
console.log(JSON.stringify({ actualFactoryResult: result }));
for (const toolName of ["read", "write", "edit", "bash", "provefab_read"]) {
  assert.equal((await handlers.get("tool_call")({ toolName, input: { path: "ordinary.txt" } }, context)).block, true);
}
const runtime = process.env.PROVEFAB_SANDBOX_RUNTIME;
process.env.PROVEFAB_SANDBOX_RUNTIME += "-absent";
const command = "touch '" + process.env.NATIVE_MARKER.replaceAll("'", "'\\''") + "'";
await assert.rejects(tools.get("provefab_bash").execute("missing-runtime", { command }, undefined, undefined, context));
process.env.PROVEFAB_SANDBOX_RUNTIME = runtime;
const policy = process.env.PROVEFAB_SANDBOX_POLICY;
const bytes = readFileSync(policy);
unlinkSync(policy);
try {
  await assert.rejects(tools.get("provefab_bash").execute("missing-policy", { command }, undefined, undefined, context));
} finally { writeFileSync(policy, bytes); }
assert.equal(existsSync(process.env.NATIVE_MARKER), false);
console.log(JSON.stringify({ missingRuntimeRefused: true, missingPolicyRefused: true, markerAbsent: true, directFactoriesAbsent: true }));
