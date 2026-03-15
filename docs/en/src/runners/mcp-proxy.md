# MCP Proxy Functionality

Model Context Protocol (MCP) is a standard communication protocol between LLM applications and tools. jobworkerp-rs's MCP proxy functionality enables you to:

- Execute MCP tools (LLMs, time information retrieval, web page fetching, etc.) provided by various MCP servers as Runners
- Run MCP tools as asynchronous jobs and retrieve their results
- Configure multiple MCP servers and combine different tools

## MCP Server Configuration

MCP server configurations are defined in a TOML file. Here's an example configuration:

```toml
[[server]]
name = "time"
description = "timezone"
transport ="stdio"
command = "uvx"
args = ["mcp-server-time", "--local-timezone=Asia/Tokyo"]

[[server]]
name = "fetch"
description = "fetch web page as markdown from web"
transport ="stdio"
command = "uvx"
args = ["mcp-server-fetch"]

# SSE transport example
#[[server]]
#name = "test-server"
#transport ="sse"
#url = "http://localhost:8080"
```

Place the configuration file at the path specified by the `MCP_CONFIG` environment variable. If the environment variable is not set, the default path `mcp-settings.toml` will be used. (If the file does not exist, it will be ignored.)

> **Note**: The configuration file can be written in TOML, JSON, or YAML format. The system attempts to parse in TOML → JSON → YAML order.

## MCP Proxy Usage Examples

1. Prepare an MCP server configuration file
2. When creating a worker, specify the numeric ID of the specific MCP server as runner_id (you can check available MCP servers using the `jobworkerp-client runner list` command)
3. Specify tool_name and arg_json in job execution arguments

```shell
# First, check the available runner-id
$ ./target/release/jobworkerp-client runner list
# Identify the specific MCP server's ID here (e.g., 3 for "time" server)

# Create a worker that uses an MCP server (e.g., time information retrieval)
# Specify the MCP server's ID number confirmed above for runner-id
$ ./target/release/jobworkerp-client worker create --name "TimeInfo" --description "" --runner-id <runner id> --response-type DIRECT --settings '' --use-static

# Execute a job to retrieve current time information
$ ./target/release/jobworkerp-client job enqueue --worker "TimeInfo" --args '{"tool_name":"get_current_time","arg_json":"{\"timezone\":\"Asia/Tokyo\"}"}'

# Web page fetching example
$ ./target/release/jobworkerp-client worker create --name "WebFetch" --description "" --response-type DIRECT --settings '' --runner-id <runner id>
$ ./target/release/jobworkerp-client job enqueue --worker "WebFetch" --args '{"tool_name":"fetch","arg_json":"{\"url\":\"https://example.com\"}"}'
```

Responses from the MCP server can be retrieved as job results and processed directly or asynchronously according to the response_type setting.

> **Note**: Runners that take time to initialize, such as PYTHON_COMMAND or MCP server tools (stdio), can be reused without reinitializing the MCP server process for each tool execution by setting the `use_static` option to `true` when creating a worker. This increases memory usage but improves execution speed.

For detailed MCP protocol specifications, refer to the [official documentation](https://modelcontextprotocol.io/).
For information about the MCP server samples used above, refer to the [official documentation](https://github.com/modelcontextprotocol/servers).
