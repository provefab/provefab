// pi-provefab: the only Pi extension Provefab loads (spec D16).
// Every tool call goes through `provefab guard`; the policy itself lives in Rust (D15).
// When the stage has an output schema, a `submit_result` tool ends the run with a typed answer.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const GUARD_TIMEOUT_MS = 10_000;

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		const bin = process.env.PROVEFAB_BIN;
		if (!bin) {
			return { block: true, reason: "PROVEFAB_BIN is not set, so Provefab guard cannot check this call" };
		}
		const res = spawnSync(bin, ["guard", "--format", "pi"], {
			input: JSON.stringify({ tool: event.toolName, args: event.input, cwd: ctx.cwd }),
			encoding: "utf8",
			timeout: GUARD_TIMEOUT_MS,
		});
		// Fail closed: anything but an explicit allow blocks the call.
		try {
			const out = JSON.parse(res.stdout ?? "");
			if (out.decision === "allow") {
				return undefined;
			}
			return { block: true, reason: String(out.reason ?? "denied by Provefab guard") };
		} catch {
			return { block: true, reason: `provefab guard failed: ${res.stderr || res.error || "no output"}` };
		}
	});

	const schemaPath = process.env.PROVEFAB_OUTPUT_SCHEMA;
	if (schemaPath) {
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
