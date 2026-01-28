# Workflow Runner

[Japanese ver.](WORKFLOW_ja.md)

The Workflow Runner is a feature that allows executing multiple jobs in a defined order or executing reusable workflows. This feature is based on [Serverless Workflow](https://serverlessworkflow.io/) (v1.0.0), with some features removed and jobworkerp-rs-specific extensions added (runner and worker for run tasks). ([Details (schema)](runner/schema/workflow.yaml))

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

- Registers a workflow definition as a reusable worker
- The created worker can then be executed repeatedly with different inputs using the `run` method

### Runner Settings (WorkflowRunnerSettings)

Settings configured in `worker.runner_settings` for the `run` method:

| Field | Type | Description |
|-------|------|-------------|
| `workflow_url` | string (oneof) | Path or URL to the workflow definition file (JSON or YAML) |
| `workflow_data` | string (oneof) | Inline workflow definition as JSON or YAML string |

> `workflow_url` and `workflow_data` are mutually exclusive (`oneof workflow_source`). Settings are optional when workflow source is provided in job arguments.

### Job Arguments

#### For `run` method (WorkflowRunArgs)

| Field | Type | Description |
|-------|------|-------------|
| `workflow_url` | string (oneof, optional) | Override workflow source with URL/path |
| `workflow_data` | string (oneof, optional) | Override workflow source with inline definition |
| `input` | string | Input for the workflow context (JSON or plain string) |
| `workflow_context` | string (optional) | Additional context information (JSON) |
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
| `status` | WorkflowStatus | Execution status: `Completed`, `Faulted`, `Cancelled`, `Running`, `Waiting` |
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
  id: 1
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
        runner:
          name: COMMAND
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
              runner:
                name: COMMAND
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

## Schema Definition

The workflow schema is defined in [runner/schema/workflow.yaml](runner/schema/workflow.yaml).

## Deprecated Runners

The following runners are deprecated. Use the `WORKFLOW` runner with the `using` parameter instead:

| Deprecated Runner | Replacement |
|-------------------|-------------|
| `INLINE_WORKFLOW` (ID: 65535) | `WORKFLOW` with `using: "run"` and workflow source in args |
| `REUSABLE_WORKFLOW` (ID: 65532) | `WORKFLOW` with `using: "run"` and workflow source in settings |
| `CREATE_WORKFLOW` (ID: -1) | `WORKFLOW` with `using: "create"` |

## Related Documentation

- [Streaming](STREAMING.md) - Streaming execution for workflow steps
