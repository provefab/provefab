// pi-provefab: the only Pi extension Provefab loads (spec D16).
// Every tool call goes through `provefab guard`; the policy itself lives in Rust (D15).
// When the stage has an output schema, a `submit_result` tool ends the run with a typed answer.
import {
	createBashToolDefinition, createLocalBashOperations, type ExtensionAPI,
} from "@earendil-works/pi-coding-agent";
import { spawnSync } from "node:child_process";
import { readFileSync, realpathSync, accessSync, constants } from "node:fs";
import { isAbsolute, relative } from "node:path";

const GUARD_TIMEOUT_MS = 10_000;

const quote = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
const safeEnvironment = (env: NodeJS.ProcessEnv): NodeJS.ProcessEnv => Object.fromEntries(
	Object.entries(env).filter(([key]) => ["HOME", "PATH", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ", "TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"].includes(key)),
);
function sandboxPaths() {
	const config = process.env.PROVEFAB_SANDBOX_POLICY;
	const runtime = process.env.PROVEFAB_SANDBOX_RUNTIME;
	const worktree = process.env.PROVEFAB_WORKTREE;
	if (!config || !runtime || !worktree) throw new Error("Provefab sandbox policy/runtime/worktree missing");
	const root = realpathSync(worktree);
	for (const path of [config, runtime]) {
		if (!isAbsolute(path)) throw new Error("Provefab sandbox paths must be absolute");
		const rel = relative(root, realpathSync(path));
		if (rel === "" || (!rel.startsWith("../") && !isAbsolute(rel))) throw new Error("Provefab sandbox configuration/runtime is writable by the worktree");
	}
	accessSync(runtime, constants.X_OK);
	const policy = JSON.parse(readFileSync(config, "utf8"));
	if (policy.network?.allowLocalBinding !== false || policy.network?.allowedDomains?.length !== 0 || !Array.isArray(policy.filesystem?.denyRead) || !Array.isArray(policy.filesystem?.denyWrite) || !Array.isArray(policy.filesystem?.allowWrite)) {
		throw new Error("Invalid Provefab sandbox policy");
	}
	return {config, runtime};
}
function guard(tool: string, args: unknown, cwd: string) {
	const bin = process.env.PROVEFAB_BIN;
	if (!bin) throw new Error("PROVEFAB_BIN is not set, so Provefab guard cannot check this call");
	const res = spawnSync(bin, ["guard", "--format", "pi"], {
		input: JSON.stringify({ tool: tool.replace(/^provefab_/, ""), args, cwd }),
		encoding: "utf8", timeout: GUARD_TIMEOUT_MS,
	});
	let out;
	try { out = JSON.parse(res.stdout ?? ""); } catch { throw new Error(`provefab guard failed: ${res.stderr || res.error || "no output"}`); }
	if (res.status !== 0 || out.decision !== "allow") throw new Error(String(out.reason ?? "denied by Provefab guard"));
}

export default function (pi: ExtensionAPI) {

	// Unique names prevent an extension-load error from restoring unguarded builtins.
	const cwd = process.env.PROVEFAB_WORKTREE || process.cwd();
	const bash = createBashToolDefinition(cwd, {
		operations: createLocalBashOperations({shellPath: "/bin/bash"}),
		exposeSessionEnvironment: false,
		spawnHook: ({command, cwd, env}) => {
			const {config, runtime} = sandboxPaths();
			return {cwd, env: safeEnvironment(env), command: `exec ${quote(runtime)} --settings ${quote(config)} -- /bin/bash --noprofile --norc -c ${quote(command)}`};
		},
	});
	// Native file factories open paths after the guard, outside SRT. Do not register them.
	const definitions = [bash];
	const permitted = new Set(["provefab_bash"]);
	for (const definition of definitions) {
		const execute = definition.execute;
		pi.registerTool({
			...definition,
			name: `provefab_${definition.name}`,
			async execute(id, params, signal, update, ctx) {
				sandboxPaths();
				guard(definition.name, params, ctx.cwd);
				return execute(id, params, signal, update, ctx);
			},
		} as typeof definition);
	}
	pi.on("user_bash", async () => ({result: {output: "Use the Provefab guarded Bash tool", exitCode: 1, cancelled: false, truncated: false}}));
	pi.on("tool_call", async (event, ctx) => {
		if (!permitted.has(event.toolName)) return {block: true, reason: "Only Provefab sandboxed Bash and the structured answer are available"};
		try { guard(event.toolName, event.input, ctx.cwd); return undefined; }
		catch (error) { return {block: true, reason: String(error)}; }
	});

	const schemaPath = process.env.PROVEFAB_OUTPUT_SCHEMA;
	if (schemaPath) {
		permitted.add("submit_result");
		const parameters = JSON.parse(readFileSync(schemaPath, "utf8"));
		pi.registerTool({
			name: "submit_result",
			label: "Submit result",
			description:
				"Submit this stage's final answer. Call it exactly once, as your last action, with every required field filled in.",
			promptSnippet: "Finish the stage by calling submit_result with the structured answer",
			promptGuidelines: [
				"Call submit_result as your final action. Do not write another message after it.",
			],
			parameters,
			async execute(_toolCallId: string, params: unknown) {
				return {
					content: [{ type: "text", text: "Result recorded." }],
					details: params,
					terminate: true,
				};
			},
		});
	}
}
