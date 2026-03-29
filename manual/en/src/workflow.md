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

- This is a convenience method that internally calls `WorkerService/Create` with Runner=REUSABLE_WORKFLOW. It automates workflow definition validation and `WorkerData` construction, but the resulting Worker is stored in the same storage as any other Worker (no special storage is used).
- Equivalent to manually creating a Worker via `WorkerService/Create` with `runner_id` set to REUSABLE_WORKFLOW and `runner_settings` containing the workflow definition.
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
- `run` task: Adopts the same structure as the official `run.script` (language, code/source, arguments, environment)
- Runtime expressions: jq syntax (`${}`) for data transformation (plus Liquid template syntax (`$${}`) as a custom extension)
- Flow control: Flow control via the `then` property (`continue`, `exit`, `end`, `wait` directives)
- Input/output transformation: `input.from`, `output.as`, `export.as`

**Not Adopted:**
- **Scheduling & event-driven triggers**: The official `schedule` (cron, interval), `listen`/`emit` (CloudEvents-based event waiting/emission) are not supported. Job scheduling is handled by jobworkerp-rs's [periodic job execution feature](operations.md)
- **External service calls (call task)**: The official specification standardizes HTTP, gRPC, AsyncAPI, and OpenAPI protocols via the `call` task. In jobworkerp-rs, these are executed through `run` task's function/runner/worker using built-in Runners (HTTP_REQUEST, GRPC, etc.), MCP servers, or plugins
- **run.container / run.shell**: The official container execution (`run.container`) and shell command execution (`run.shell`) are not supported. Equivalent functionality is available through the DOCKER runner and COMMAND runner via `run.function`/`run.runner`
- **run.workflow**: The official nested workflow execution (`run.workflow`) is not supported. Use the WORKFLOW runner via `run.function`/`run.runner` instead
- **await / return**: The `await` (completion waiting) and `return` (stdout/stderr/code/all/none) properties of the official `run` task exist in the schema definition but are not implemented in the execution code
- **Authentication & secret management**: `use.authentications` and the `$secrets` variable are not supported. Authentication credentials are managed through Runner settings or environment variables
- **Catalogs & extensions**: `use.catalogs` (external collections of reusable components), `use.extensions` (pre/post task processing injection), and `use.functions` (external function definitions) are not supported
- **Lifecycle events**: CloudEvents-based workflow/task state change notifications are not supported. Instead, real-time monitoring is available via gRPC streaming

**jobworkerp-rs-specific Extensions:**
- `run.function` / `run.runner` / `run.worker`: Unified access to jobworkerp-rs's job execution platform from workflows (built-in Runners, MCP servers, plugins, etc.). These serve as replacements for the official `call` task, `run.container`, `run.shell`, and `run.workflow`
- `useStreaming` for streaming execution (progress monitoring for long-running tasks such as LLMs)
- `checkpoint` for checkpoint and restart functionality
- Liquid template syntax (`$${}`) for string templating

## Related Documentation

- [Streaming](streaming.md) - Streaming execution for workflow steps
