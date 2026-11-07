import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const here = dirname(fileURLToPath(import.meta.url));
const sdkRoot = process.env.AG_UI_SDK_ROOT;
if (!sdkRoot) {
  throw new Error(
    "Set AG_UI_SDK_ROOT to a package directory whose node_modules contain @ag-ui/client and @ag-ui/core 1.0.2.",
  );
}

const sdkRequire = createRequire(resolve(sdkRoot, "_ag_ui_compat_test.cjs"));
const { HttpAgent } = sdkRequire("@ag-ui/client");
const { EventSchemas, RunAgentInputSchema } = sdkRequire("@ag-ui/core/schemas");
const fixture = JSON.parse(
  await readFile(resolve(here, "fixtures/ag-ui-sdk-1.0.2.json"), "utf8"),
);

function sse(events) {
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function agentFor(threadId, eventBatches, requests = []) {
  let batch = 0;
  return new HttpAgent({
    url: "http://ag-ui-fixture.invalid/ag-ui/run",
    threadId,
    fetch: async (_url, init) => {
      requests.push(JSON.parse(init.body));
      const events = eventBatches[batch++];
      assert.ok(events, "SDK requested an unexpected extra run");
      return new Response(sse(events), {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    },
  });
}

test("AG-UI SDK 1.0.2 validates the AG-UI wire fixtures", async () => {
  const clientPackage = JSON.parse(
    await readFile(resolve(sdkRoot, "node_modules/@ag-ui/client/package.json"), "utf8"),
  );
  const corePackage = JSON.parse(
    await readFile(resolve(sdkRoot, "node_modules/@ag-ui/core/package.json"), "utf8"),
  );
  assert.equal(clientPackage.version, "1.0.2");
  assert.equal(corePackage.version, "1.0.2");

  for (const events of Object.values(fixture)) {
    for (const event of events) EventSchemas.parse(event);
  }
  RunAgentInputSchema.parse({
    threadId: "thread-tools",
    runId: "run-tools-2",
    messages: [
      {
        id: "assistant-1",
        role: "assistant",
        toolCalls: [
          {
            id: "call-1",
            type: "function",
            function: { name: "computer_click", arguments: "{\"x\":1}" },
          },
        ],
      },
      { id: "tool-msg-1", role: "tool", toolCallId: "call-1", content: "clicked" },
    ],
    resume: [{ interruptId: "approval-1", status: "resolved" }],
  });
});

test("AG-UI SDK schema decoder preserves unknown fields instead of rejecting them", () => {
  const event = EventSchemas.parse({
    type: "RUN_FINISHED",
    threadId: "thread-text",
    runId: "run-text",
    outcome: { type: "success" },
    proprietaryField: true,
  });
  assert.equal(event.proprietaryField, true);
});

test("HttpAgent decodes a complete text run and retains the assistant message", async () => {
  const agent = agentFor("thread-text", [fixture.text]);
  const result = await agent.runAgent({ runId: "run-text" });
  assert.deepEqual(result.newMessages, [
    { id: "message-text", role: "assistant", content: "hello" },
  ]);
  assert.equal(agent.messages.at(-1).content, "hello");
});

test("HttpAgent retains a client tool call and sends its result on the following run", async () => {
  const requests = [];
  const agent = agentFor(
    "thread-tools",
    [fixture.clientToolCall, fixture.clientToolResult],
    requests,
  );
  const first = await agent.runAgent({ runId: "run-tools-1" });
  assert.deepEqual(first.newMessages[0].toolCalls, [
    {
      id: "call-1",
      type: "function",
      function: { name: "computer_click", arguments: "{\"x\":1}" },
    },
  ]);

  agent.messages.push({
    id: "tool-msg-1",
    role: "tool",
    toolCallId: "call-1",
    content: "clicked",
  });
  await agent.runAgent({ runId: "run-tools-2" });
  assert.ok(
    requests[1].messages.some(
      (message) => message.role === "tool" && message.toolCallId === "call-1",
    ),
  );
});

test("HttpAgent tracks standard interrupts and sends the answer with the next run", async () => {
  const requests = [];
  const agent = agentFor("thread-resume", [fixture.interrupt, fixture.resumed], requests);
  await agent.runAgent({ runId: "run-resume-1" });
  assert.deepEqual(agent.pendingInterrupts.map(({ id }) => id), ["approval-1"]);

  await agent.runAgent({
    runId: "run-resume-2",
    resume: [{ interruptId: "approval-1", status: "resolved", payload: { approved: true } }],
  });
  assert.deepEqual(requests[1].resume, [
    { interruptId: "approval-1", status: "resolved", payload: { approved: true } },
  ]);
  assert.deepEqual(agent.pendingInterrupts, []);
});
