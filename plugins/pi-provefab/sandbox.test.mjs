import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, chmodSync, rmSync, existsSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { stripTypeScriptTypes } from "node:module";
import vm from "node:vm";

// Exercise the actual installed Pi factories; no model/API call is needed.
const piPath = process.env.PI_TEST_MODULE;
assert.ok(piPath, "Set PI_TEST_MODULE to the installed Pi dist/index.js");
const factory = await import(pathToFileURL(piPath).href);
const source = stripTypeScriptTypes(readFileSync(new URL("extensions/provefab.ts", import.meta.url), "utf8"));
const module = new vm.SourceTextModule(source);
await module.link(async (specifier) => {
  const exports = specifier === "@earendil-works/pi-coding-agent" ? factory : await import(specifier);
  return new vm.SyntheticModule(Object.keys(exports), function () { for (const [key,value] of Object.entries(exports)) this.setExport(key,value); });
});
await module.evaluate();
function load() {
  const tools = new Map(), handlers = new Map();
  module.namespace.default({ registerTool: t => tools.set(t.name,t), on: (name,handler) => handlers.set(name,handler) });
  return {tools,handlers};
}
test("all exposed Pi tools require the successfully loaded extension", () => {
  const { tools } = load();
  for (const name of ["read","write","edit","grep","find","ls","bash"]) {
    assert.ok(tools.has(`provefab_${name}`), `${name} must not fall back to a builtin`);
  }
});
test("missing policy prevents shell execution", async () => {
  delete process.env.PROVEFAB_SANDBOX_POLICY;
  delete process.env.PROVEFAB_SANDBOX_RUNTIME;
  const {tools} = load();
  const bash = tools.get("provefab_bash");
  assert.ok(bash, "guarded Bash registered");
  await assert.rejects(bash.execute("1", {command: "printf escaped"}, undefined, undefined, {cwd: process.cwd()}), /sandbox/i);
});
test("user_bash cannot bypass the guarded tool", async () => {
  const {handlers} = load();
  assert.ok(handlers.has("user_bash"));
  const result = await handlers.get("user_bash")({command:"true"},{});
  assert.notEqual(result.result.exitCode, 0);
});
test("wrapper quotes commands and retains timeout and cancellation", async () => {
  const root = mkdtempSync(join(tmpdir(),"provefab-pi-"));
  try {
    const work=join(root,"work"); mkdirSync(work);
    const runtime=join(root,"srt ' quoted"), policy=join(root,"policy ' quoted.json"), guard=join(root,"guard");
    writeFileSync(policy, JSON.stringify({network:{allowedDomains:[],allowLocalBinding:false},filesystem:{allowWrite:[root],denyRead:[],denyWrite:[]}}));
    writeFileSync(runtime, '#!/bin/sh\n[ "$1" = --settings ] || exit 61\n[ -f "$2" ] || exit 62\nshift 2\n[ "$1" = -- ] || exit 63\nshift\nexec "$@"\n'); chmodSync(runtime,0o755);
    writeFileSync(guard, `#!/bin/sh\ncat >/dev/null\nprintf '{"decision":"allow"}\n'\n`); chmodSync(guard,0o755);
    Object.assign(process.env, {PROVEFAB_SANDBOX_POLICY:policy,PROVEFAB_SANDBOX_RUNTIME:runtime,PROVEFAB_BIN:guard,PROVEFAB_WORKTREE:work,FAKE_SECRET_TOKEN:"must-not-leak",BASH_ENV:join(root,"startup")});
    writeFileSync(process.env.BASH_ENV,'touch "'+join(root,"startup-ran")+'"');
    const {tools,handlers}=load(); const bash=tools.get("provefab_bash");
    const context={cwd:work};
    assert.equal(await handlers.get("tool_call")({toolName:"provefab_bash",input:{command:"printf test"}},context),undefined);
    const result=await bash.execute("1",{command:"printf '%s' \"a'b $FAKE_SECRET_TOKEN\"",timeout:2},undefined,undefined,context);
    assert.match(JSON.stringify(result),/a'b /); assert.doesNotMatch(JSON.stringify(result),/must-not-leak/);
    assert.equal(existsSync(join(root,"startup-ran")),false);
    await assert.rejects(bash.execute("2",{command:"sleep 10",timeout:0.05},undefined,undefined,context),/timed out|timeout/i);
    const controller=new AbortController(); setTimeout(()=>controller.abort(),50);
    await assert.rejects(bash.execute("3",{command:"sleep 10"},controller.signal,undefined,context),/abort/i);
  } finally { rmSync(root,{recursive:true,force:true}); }
});
