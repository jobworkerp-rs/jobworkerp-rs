/**
 * Tool call types for HITL (Human-in-the-Loop) support.
 * Used for LLM tool call approval/rejection UI.
 */

/** Status of a tool call */
export type ToolCallStatus = 'pending' | 'approved' | 'rejected' | 'executing' | 'completed';

/** Single tool call information */
export interface ToolCall {
    /** Unique identifier for this tool call */
    id: string;
    /** Name of the function/tool being called */
    name: string;
    /** Arguments passed to the tool (parsed JSON) */
    arguments: Record<string, unknown>;
    /** Raw arguments string (for display/editing) */
    argumentsRaw: string;
    /** Current status of this tool call */
    status: ToolCallStatus;
    /** Result after execution (if completed) */
    result?: unknown;
    /** Parent message ID (if associated with a text message) */
    parentMessageId?: string;
}

/** State for pending tool calls awaiting client approval */
export interface PendingToolCallsState {
    /** Run ID associated with these tool calls */
    runId: string;
    /** List of pending tool calls */
    toolCalls: ToolCall[];
}

/** Result to send back when approving a tool call */
export interface ToolCallResult {
    /** Tool call ID (must match the one from TOOL_CALL_START) */
    toolCallId: string;
    /** Result value to return */
    result: unknown;
}

/** Request body for /ag-ui/message endpoint */
export interface HitlMessageRequest {
    /** Run ID to resume */
    runId: string;
    /** Tool call results */
    toolCallResults: ToolCallResult[];
}

/** AG-UI event types related to tool calls */
export type AgUiToolEventType =
    | 'TOOL_CALL_START'
    | 'TOOL_CALL_ARGS'
    | 'TOOL_CALL_END'
    | 'TOOL_CALL_RESULT';

/** TOOL_CALL_START event payload */
export interface ToolCallStartEvent {
    type: 'TOOL_CALL_START';
    toolCallId: string;
    toolCallName: string;
    parentMessageId?: string;
}

/** TOOL_CALL_ARGS event payload */
export interface ToolCallArgsEvent {
    type: 'TOOL_CALL_ARGS';
    toolCallId: string;
    delta: string;
}

/** TOOL_CALL_END event payload */
export interface ToolCallEndEvent {
    type: 'TOOL_CALL_END';
    toolCallId: string;
}

/** TOOL_CALL_RESULT event payload */
export interface ToolCallResultEvent {
    type: 'TOOL_CALL_RESULT';
    toolCallId: string;
    result: unknown;
}

/** Union type for all tool-related events */
export type ToolCallEvent =
    | ToolCallStartEvent
    | ToolCallArgsEvent
    | ToolCallEndEvent
    | ToolCallResultEvent;

/** Check if an event is a tool call event */
export function isToolCallEvent(event: { type?: string }): event is ToolCallEvent {
    return (
        event.type === 'TOOL_CALL_START' ||
        event.type === 'TOOL_CALL_ARGS' ||
        event.type === 'TOOL_CALL_END' ||
        event.type === 'TOOL_CALL_RESULT'
    );
}
