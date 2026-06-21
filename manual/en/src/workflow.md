# Workflow Runner

The Workflow Runner is a feature that allows executing multiple jobs in a defined order or executing reusable workflows. It conforms to the workflow logic construction methods of [Serverless Workflow](https://serverlessworkflow.io/) ([DSL Specification v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl.md)) — sequential task execution, branching, loops, error handling, and runtime expressions — while extending them as a custom DSL integrated with the jobworkerp-rs job execution platform. ([jobworkerp-rs Extended Schema](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml))

## WORKFLOW Runner (Unified)

The `WORKFLOW` runner is a multi-method runner that provides workflow capabilities through jobworkerp-rs. It uses the `using` parameter to specify the execution method.

### Methods

#### run (Default)

Executes a workflow definition. This is the default method when `using` is not specified.

- Workflow can be pre-configured in `worker.runner_settings` or specified at runtime via job arguments
- When both are provided, job arguments take precedence over settings
- Supports dynamic variable expansion using both jq syntax (`${}`) and Liquid template syntax (`$${}`)
- Supports streaming execution for real-time monitoring
- Supports checkpoint-based resume

#### create

Creates a new WORKFLOW runner worker from a workflow definition.

- This is a convenience method that internally calls `WorkerService/Create` with Runner=WORKFLOW. It automates workflow definition validation and `WorkerData` construction, but the resulting Worker is stored in the same storage as any other Worker (no special storage is used).
- Equivalent to manually creating a Worker via `WorkerService/Create` with `runner_id` set to WORKFLOW and `runner_settings` containing the workflow definition.
- The created worker can then be executed repeatedly with different inputs using the `run` method

### Runner Settings (WorkflowRunnerSettings)

Settings configured in `worker.runner_settings` for the `run` method:

| Field | Type | Description |
|-------|------|-------------|
| `workflow_url` | string (oneof) | Path or URL to the workflow definition file (JSON or YAML) |
| `workflow_data` | string (oneof) | Inline workflow definition as JSON or YAML string |
| `workflow_context` | string (optional) | Context variables (JSON object string). If set, `workflow_context` in runtime args is ignored |

> `workflow_url` and `workflow_data` are mutually exclusive (`oneof workflow_source`). Settings are optional when workflow source is provided in job arguments.

### Job Arguments

#### For `run` method (WorkflowRunArgs)

| Field | Type | Description |
|-------|------|-------------|
| `workflow_url` | string (oneof, optional) | Override workflow source with URL/path |
| `workflow_data` | string (oneof, optional) | Override workflow source with inline definition |
| `input` | string | Input for the workflow context (JSON or plain string) |
| `workflow_context` | string (optional) | Additional context information (JSON). Ignored if `workflow_context` is set in runner settings |
| `execution_id` | string (optional) | Unique identifier for the workflow execution instance |
| `from_checkpoint` | Checkpoint (optional) | Data for resuming from a specific checkpoint |

#### For `create` method (CreateWorkflowArgs)

| Field | Type | Description |
|-------|------|-------------|
| `workflow_url` | string (oneof) | Workflow source URL/path |
| `workflow_data` | string (oneof) | Inline workflow definition |
| `name` | string | Name for the worker to be created |
| `worker_options` | WorkerOptions (optional) | Worker configuration (retry policy, channel, response type, etc.) |

### Result Types

#### WorkflowResult (for `run` method)

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique identifier for the workflow execution (UUID v7) |
| `output` | string | Result of the workflow execution (JSON or plain string) |
| `position` | string | Execution position in JSON Pointer format |
| `status` | WorkflowStatus | Execution status: `Completed`, `Faulted`, `Cancelled`, `Running`, `Waiting`, `Pending` (internal status, typically not seen in final results) |
| `error_message` | string (optional) | Error details if the workflow failed |

#### CreateWorkflowResult (for `create` method)

| Field | Type | Description |
|-------|------|-------------|
| `worker_id` | WorkerId (optional) | Identifier of the created worker |

## Workflow Example

Below is an example of a workflow that lists files and further processes directories:
(`$${...}`: Liquid template, `${...}`: jq)

```yaml
document:
  name: ls-test
  namespace: default
  title: Workflow test (ls)
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    document:
      type: string
      description: file name
      default: /
do:
  - ListWorker:
      run:
        function:
          runnerName: COMMAND
          arguments:
            command: ls
            args: ["${.}"]
          options:
            channel: workflow
            useStatic: false
            storeSuccess: true
            storeFailure: true
      output:
        as: |-
          $${
          {%- assign files = stdout | newline_to_br | split: '<br />' -%}
          {"files": [
          {%- for file in files -%}
          "{{- file |strip_newlines -}}"{% unless forloop.last %},{% endunless -%}
          {%- endfor -%}
          ] }
          }
  - EachFileIteration:
      for:
        each: file
        in: ${.files}
        at: ind
      do:
        - ListWorkerInner:
            if: |-
              $${{%- assign head_char = file | slice: 0, 1 -%}{%- if head_char == "d" %}true{% else %}false{% endif -%}}
            run:
              function:
                runnerName: COMMAND
                arguments:
                  command: ls
                  args: ["$${/{{file}}}"]
                options:
                  channel: workflow
                  useStatic: false
                  storeSuccess: true
                  storeFailure: true
```

The root `input` section is optional. When it is omitted, jobworkerp-rs does not
apply input schema validation or `input.from` transformation, and the runtime
input is passed to the workflow tasks unchanged.

## How to Use the Workflow Runner

### Using gRPC API

The WORKFLOW runner is accessed via gRPC `JobService.Enqueue` (or `EnqueueForStream` for streaming) with:

- `worker_name`: Name of the worker created with WORKFLOW runner
- `args`: Serialized `WorkflowRunArgs` or `CreateWorkflowArgs` (protobuf)
- `using`: `"run"` (default, can be omitted) or `"create"`

### Using jobworkerp-client CLI

Save the above workflow definition as `ls.yaml` in the same directory as the worker process:

```shell
# Create a WORKFLOW worker with pre-configured workflow (run method)
$ ./target/release/jobworkerp-client worker create \
    --name "MyWorkflow" \
    --runner-name WORKFLOW \
    --response-type DIRECT \
    --settings '{"workflow_url":"./ls.yaml"}'

# Execute workflow with input
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"input":"/home"}'

# Execute workflow with runtime workflow override
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"workflow_url":"./another.yaml", "input":"/tmp"}'

# Create a reusable workflow worker (create method)
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --using create \
    --args '{"workflow_data":"<YAML or JSON workflow definition>", "name":"new-workflow-worker"}'

# Execute workflow directly without creating a worker (shortcut)
$ ./target/release/jobworkerp-client job enqueue-workflow -i '/path/to/list' -w ./ls.yaml
# This command automatically creates a temporary worker, executes the workflow, and deletes the worker
```

> **Note**: `workflow_url` can specify not only URLs like `https://` but also absolute/relative paths to files on the local filesystem. If a relative path is specified, it must be relative to the execution directory of jobworkerp-worker.

## Variable Expansion

Workflows support two types of variable expansion:

- **jq syntax** (`${...}`): Standard jq expressions for JSON data manipulation
- **Liquid template syntax** (`$${...}`): [Liquid](https://shopify.github.io/liquid/) template expressions for string manipulation and control flow

### Environment Variable Access

In jq syntax, you can access process environment variables via the built-in `env` function (provided by the jaq crate):

```yaml
# Accessing environment variables in jq syntax
botToken: ${env.SLACK_BOT_TOKEN}
apiKey: ${env.API_KEY}
```

> **Note**: Environment variable access is only available in jq syntax (`${...}`). Liquid template syntax (`$${...}`) does not support direct environment variable access.

### Context Variables

You can pass global variables via the `workflow_context` parameter separately from `input` when executing a workflow. Each key of the provided JSON object becomes a top-level variable accessible throughout the entire workflow.

#### How to Pass

**Via runner settings (WorkflowRunnerSettings):**

Set `workflow_context` as a JSON string in `runner_settings` when creating the worker. When set in settings, `workflow_context` in runtime args is ignored.

```shell
# Set context variables in worker settings
$ ./target/release/jobworkerp-client worker create \
    --name "MyWorkflow" \
    --runner-name WORKFLOW \
    --response-type DIRECT \
    --settings '{"workflow_url":"./my-workflow.yaml", "workflow_context":"{\"api_url\":\"https://api.example.com\",\"slack_token\":\"xoxb-xxx\"}"}'
```

**Via job arguments (WorkflowRunArgs):**

Only effective when `workflow_context` is not set in runner settings.

```shell
# Pass context variables at runtime
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"input":"/home", "workflow_context":"{\"api_url\":\"https://api.example.com\",\"max_retries\":3}"}'
```

#### How to Reference

In jq syntax use `$variable_name`, in Liquid template syntax use `{{ variable_name }}`:

```yaml
do:
  - callApi:
      run:
        function:
          runnerName: HTTP_REQUEST
          arguments:
            # jq syntax: reference with $variable_name
            url: ${"$api_url" + "/data"}
            maxRetries: ${$max_retries}
  - notify:
      run:
        function:
          runnerName: COMMAND
          arguments:
            # Liquid template syntax: reference with {{ variable_name }}
            command: "echo"
            args: ["$${API URL is {{ api_url }}}"]
```

#### Adding Context Variables with export.as

Use `export.as` on a task to save task output as context variables, making them available to subsequent tasks:

```yaml
do:
  - checkStatus:
      run:
        function:
          runnerName: COMMAND
          arguments:
            command: "test"
            args: ["-d", "/data"]
            treat_nonzero_as_error: false
      export:
        as:
          dir_exists: "${.exitCode == 0}"
  - processIfExists:
      if: "${$dir_exists == true}"
      run:
        # ...
```

## Schema Definition

- [Serverless Workflow DSL Specification v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl.md) - The base official DSL specification
- [Serverless Workflow DSL Reference v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl-reference.md) - Detailed reference for official DSL tasks and properties
- [jobworkerp-rs Extended Schema (runner/schema/workflow.yaml)](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml) - Schema definition with jobworkerp-rs-specific extensions

## Creating Workflows with Agent Skill

By installing the [jobworkerp-workflow-plugin](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin) in [Claude Code](https://docs.anthropic.com/en/docs/claude-code), the `/jobworkerp-workflow` Skill becomes available. This Skill allows you to describe workflow requirements in natural language and automatically generate YAML definitions compliant with jobworkerp-rs Custom Serverless Workflow DSL v1.0.0.

The Skill references DSL specifications including function/runner/worker task definitions, runner settings/arguments schemas, jq/Liquid variable expansion, flow control (for, switch, fork), and error handling (try-catch) to generate accurate workflows.

For setup instructions, see the [jobworkerp-workflow-plugin repository](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin).

## Differences from the Official Serverless Workflow Specification

The jobworkerp-rs workflow DSL adopts the logic construction aspects of the official specification, but does not conform to its ecosystem or operational features.

**Adopted (Flow Control & Logic Construction):**
- Task definitions: `do`, `for`, `fork`, `switch`, `try`, `set`, `raise`, `wait`, `run` (with extensions)
- `run` task: Adopts the same structure as the official `run.script` (language, code/source, arguments, environment), plus `run.shell`, `run.container`, and a jobworkerp-specific `run.workflow` source form
- `call` task: Basic `call: http` support (method, endpoint, headers, query, body, output) including endpoint `authentication` (bearer / basic), and `call: grpc` unary support (service name/host/port, method, arguments) using server reflection, with service `authentication` limited to bearer
- Runtime expressions: jq syntax (`${}`) for data transformation (plus Liquid template syntax (`$${}`) as a custom extension)
- Flow control: Flow control via the `then` property (`continue`, `exit`, `end`, `wait` directives)
- Input/output transformation: `input.from`, `output.as`, `export.as`

`run.shell`, `run.container`, and `run.workflow` are not exact copies of the official task definitions:

- `run.shell`: `withMemoryMonitoring`, `treatNonzeroAsError`, and `successExitCodes` are jobworkerp extensions. They affect monitoring and exit-code-to-task-failure handling.
- `run.container`: `timeoutSec`, `treatNonzeroAsError`, and `successExitCodes` are jobworkerp extensions. They affect job timeout and exit-code-to-task-failure handling.
- `await` is a common run option available on **every** `run.*` instance (`run.runner`, `run.function`, `run.worker`, `run.script`, `run.workflow`, `run.shell`, `run.container`). `await: false` enqueues the job fire-and-forget without waiting and continues with the current task input as the task output. The default is `await: true`. (Under `useStreaming: true`, `await: false` is rejected because streaming must collect a runner result.)
- `return` is available on the process tasks `run.script`, `run.shell`, and `run.container` because those produce a process result `{code, stdout, stderr}`; runner / function / worker / workflow have runner-defined output types and reject `return`. `return` accepts `stdout`, `stderr`, `code`, `all`, and `none`; the default is `stdout` (the **raw stdout string**), and `return: all` produces `{code, stdout, stderr}`. For `run.script`, the default `stdout` returns the raw stdout text as-is — it is no longer auto-parsed as JSON, matching the Serverless Workflow v1.0.0 process-result contract. To consume structured output, have the script print JSON and parse the returned string, or use `return: all`.
  - Exit-code handling differs between the process tasks. `run.shell` / `run.container` accept the `treatNonzeroAsError` and `successExitCodes` extensions and, by default, do **not** fail on a non-zero exit, so `return: code` and the `code` field of `return: all` can observe non-zero exit codes. `run.script` has no such option: a non-zero exit always fails the task before `return` is applied, so for scripts `return: code` / `return: all` only ever observe a successful exit (`0`).
- `run.script`: streaming is not supported (the PYTHON_COMMAND runner has no streaming output), so a script task must run with `useStreaming: false` (the default). It still runs inside a streaming workflow when its own `useStreaming` is false.
- `run.workflow`: `workflowData` / `workflowUrl` are jobworkerp extensions for specifying the nested workflow source. The official workflow process reference form is not implemented here. The parent workflow context is inherited implicitly; `workflowContext` is not a DSL field.
- `call: grpc`: adopts the official `service` (name / host / port / authentication), `method`, and `arguments` for unary calls. It is adapted to the jobworkerp GRPC runner; `arguments` is sent as a JSON body and converted to protobuf via server reflection (`use_reflection` must be enabled). The output is shaped as `{ code, body, metadata, message? }`, where `body` is the JSON-parsed response. The GRPC runner itself supports proto descriptors, but accepting the official `proto` (externalResource) in the workflow executor is deferred, so `call: grpc` still rejects it at runtime. Service `authentication` supports only bearer; basic is rejected (the GRPC runner's single `auth_token` field cannot represent it). Server streaming is not supported.

Example:

```yaml
do:
  - echo:
      run:
        shell:
          command: echo
          args: ["hello"]
  - boxed:
      run:
        container:
          image: alpine:3.20
          command: ["echo", "hello"]
  - child:
      run:
        workflow:
          workflowUrl: file:///workflows/child.yaml
          input:
            value: 1
```

**Not Adopted:**
- **Scheduling & event-driven triggers**: The official `schedule` (cron, interval), `listen`/`emit` (CloudEvents-based event waiting/emission) are not supported. Job scheduling is handled by jobworkerp-rs's [periodic job execution feature](operations.md)
- **External service calls (call task)**: `call: http` (endpoint `authentication` with the `bearer` and `basic` schemes) and `call: grpc` unary calls (service `authentication` limited to `bearer`) are supported. The `call: grpc` `proto` (externalResource, since reflection is used), basic authentication, and server streaming, plus AsyncAPI, OpenAPI, named function calls, the HTTP `digest` / `oauth2` / `oidc` / `certificate` authentication schemes, and HTTP `output: raw` are not supported
- **await / return scope**: `await` is supported on every `run.*` instance (`await: false` = fire-and-forget). `return` is available on the process tasks (script / shell / container); runner / function / worker / workflow reject it. Under `useStreaming: true`, `await: false` is rejected for all instances (and `run.script` itself does not support `useStreaming: true`).
- **Authentication & secret management**: `use.authentications` (named bearer/basic policies referenced via `authentication: { use: <name> }`), `use.secrets` (declared secret names), the official `secretBasedAuthenticationPolicy` (`bearer: { use: <name> }` / `basic: { use: <name> }`), and the `$secrets` variable are supported for `call: http` endpoint authentication. Secret values are never written in the workflow definition: declare names under `use.secrets` and supply each value through the `WORKFLOW_SECRET_<UPPER_SNAKE_NAME>` environment variable (e.g. `github-token` → `WORKFLOW_SECRET_GITHUB_TOKEN`). For a bearer secret the env value is the token string (or `{"token": "..."}` JSON); for basic it is `{"username": "...", "password": "..."}` JSON. A scheme value can be bound to a secret in three ways: (1) the official `bearer: { use: <name> }` / `basic: { use: <name> }` reference; (2) a runtime expression `bearer: { token: "${ $secrets.<name> }" }`; (3) a literal inline value (discouraged for credentials). Only names declared in `use.secrets` resolve, for both `{ use }` and `$secrets`; an undeclared reference is an error and the environment is not read (it does not silently expand to a jq `null` or an empty Liquid string). `$secrets` is injected only for `call` task evaluation and is never written back to the workflow context. `secrets` is a reserved name: a workflow input containing `{"secrets": {...}}` cannot override the `$secrets` (authentication material) seen during expression evaluation. Secret-derived `Authorization` header values are masked in debug logs. The `digest` / `oauth2` / `oidc` / `certificate` schemes are not yet supported. (Other credentials such as LLM API keys are still managed through Runner settings or environment variables.)
- **Catalogs & extensions**: `use.catalogs` (external collections of reusable components), `use.extensions` (pre/post task processing injection), and `use.functions` (external function definitions) are not supported
- **Lifecycle events**: CloudEvents-based workflow/task state change notifications are not supported. Instead, real-time monitoring is available via gRPC streaming

**jobworkerp-rs-specific Extensions:**
- `run.function` / `run.runner` / `run.worker`: Unified access to jobworkerp-rs's job execution platform from workflows (built-in Runners, MCP servers, plugins, etc.)
- `useStreaming` for streaming execution (progress monitoring for long-running tasks such as LLMs)
- `checkpoint` for checkpoint and restart functionality
- Liquid template syntax (`$${}`) for string templating

## Related Documentation

- [Streaming](streaming.md) - Streaming execution for workflow steps
