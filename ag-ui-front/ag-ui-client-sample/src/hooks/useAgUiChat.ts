
import { useState, useCallback, useRef, useEffect } from 'react';
import { runHttpRequest, parseSSEStream } from '@ag-ui/client';
import { v4 as uuidv4 } from 'uuid';
import { Subscription } from 'rxjs';
import {
    ToolCall,
    ToolCallStatus,
} from '../types/tool';

// Message type definition
export interface Message {
    id: string;
    role: 'user' | 'assistant' | 'system' | 'tool';
    content: string;
    // Tool call metadata (only for role='tool')
    toolCall?: {
        name: string;
        arguments: string;
        result: unknown;
    };
}

// Inline workflow definition for LLM_CHAT
// Streaming workflow with HITL mode (isAutoCalling: false)
// Uses ${ $runnerMessages } from workflowContext for message history
const CHAT_WORKFLOW = {
    document: {
        dsl: "1.0.0-jobworkerp",
        namespace: "default",
        name: "copilot-chat",
        version: "0.1.0",
        title: "AG-UI Chat Workflow"
    },
    input: {},
    do: [
        {
            "ChatTask": {
                useStreaming: true,
                run: {
                    runner: {
                        name: "LLM_CHAT",
                        settings: {
                            ollama: {
                                baseUrl: process.env.NEXT_PUBLIC_OLLAMA_BASE_URL || "http://localhost:11434",
                                model: process.env.NEXT_PUBLIC_OLLAMA_MODEL || "gpt-oss:20b",
                                pullModel: false
                            }
                        },
                        arguments: {
                            systemPrompt: "You are a helpful assistant. When asked to run commands, use the available tools to execute them and provide the results.",
                            messages: "${ $runnerMessages }",
                            options: {
                                maxTokens: 4000,
                                temperature: 0.7,
                                topP: 0.9
                            },
                            functionOptions: {
                                useFunctionCalling: true,
                                functionSetName: "command-functions",
                                isAutoCalling: false  // HITL mode - require user approval
                            }
                        },
                        options: {
                            channel: "llm",
                            broadcastResults: true
                        }
                    }
                }
            }
        }
    ]
};

export function useAgUiChat() {
    const [messages, setMessages] = useState<Message[]>([
        {
            id: 'welcome',
            role: 'assistant',
            content: 'Hello! I am connected to AG-UI. How can I help you today?'
        }
    ]);
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);

    // Tool call state for HITL
    const [pendingToolCalls, setPendingToolCalls] = useState<ToolCall[]>([]);
    const [currentRunId, setCurrentRunId] = useState<string | null>(null);
    const [isWaitingForApproval, setIsWaitingForApproval] = useState(false);

    // AG-UI Interrupts: Store interrupt info for resume flow
    const [currentInterruptId, setCurrentInterruptId] = useState<string | null>(null);

    // Ref to track tool calls during streaming (accumulated arguments)
    const toolCallsRef = useRef<Map<string, ToolCall>>(new Map());

    // Refs for subscription cleanup to prevent memory leaks
    const sendMessageSubscriptionRef = useRef<Subscription | null>(null);
    const approveSubscriptionRef = useRef<Subscription | null>(null);
    const rejectSubscriptionRef = useRef<Subscription | null>(null);
    const abortControllerRef = useRef<AbortController | null>(null);

    // Cleanup subscriptions on unmount
    useEffect(() => {
        return () => {
            sendMessageSubscriptionRef.current?.unsubscribe();
            approveSubscriptionRef.current?.unsubscribe();
            rejectSubscriptionRef.current?.unsubscribe();
            abortControllerRef.current?.abort();
        };
    }, []);

    // Options for sendMessage
    interface SendMessageOptions {
        // For tool result follow-up: add tool message to UI instead of user message
        toolResult?: {
            name: string;
            arguments: string;
            result: unknown;
        };
    }

    const sendMessage = useCallback(async (content: string, options?: SendMessageOptions) => {
        if (!content.trim()) return;

        const messageId = uuidv4();

        // Add message to state (tool or user)
        if (options?.toolResult) {
            // Add tool result message with special styling
            setMessages(prev => [...prev, {
                id: messageId,
                role: 'tool',
                content: `Executed: ${options.toolResult.name}`,
                toolCall: options.toolResult
            }]);
        } else {
            // Add regular user message
            setMessages(prev => [...prev, {
                id: messageId,
                role: 'user',
                content: content
            }]);
        }

        setIsLoading(true);
        setError(null);

        // Placeholder for assistant response
        const assistantMessageId = uuidv4();
        setMessages(prev => [...prev, {
            id: assistantMessageId,
            role: 'assistant',
            content: ''
        }]);

        try {
            console.log("Sending request to AG-UI Proxy with content:", content);

            // Prepare history (for AG-UI protocol)
            const history = messages.filter(m => m.id !== 'welcome').map(m => ({
                id: m.id,
                role: m.role === 'tool' ? 'user' : m.role,  // Map tool to user for history
                content: m.content
            }));

            // Prepare runner-compliant messages (for LLM)
            const runnerMessages = [
                ...messages.filter(m => m.id !== 'welcome').map(m => ({
                    role: m.role.toUpperCase(),
                    content: { text: m.content }
                })),
                // Add current message with appropriate role
                {
                    role: options?.toolResult ? 'TOOL' : 'USER',
                    content: { text: content }
                }
            ];

            // Add to history
            history.push({
                id: messageId,
                role: options?.toolResult ? 'tool' : 'user',
                content: content
            });

            // Prepare input payload using CHAT_WORKFLOW with workflowContext
            const input = {
                context: [{
                    type: "workflow_inline",
                    data: {
                        workflow: CHAT_WORKFLOW,
                        // workflowContext must be a JSON formatted string according to proto
                        workflowContext: JSON.stringify({
                            runnerMessages: runnerMessages
                        })
                    }
                }],
                messages: history
            };

            // Use direct backend URL to avoid Next.js rewrite buffering issue
            // Next.js rewrites buffer SSE streams, causing all events to arrive at once
            const baseUrl = process.env.NEXT_PUBLIC_AG_UI_BASE_URL || 'http://localhost:8080';
            const url = `${baseUrl}/ag-ui/run`;

            console.log("Fetching URL:", url);

            const requestInit = {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Accept': 'text/event-stream' // Important for SSE
                },
                body: JSON.stringify(input)
            };

            // Cleanup previous subscription before creating new one
            sendMessageSubscriptionRef.current?.unsubscribe();
            abortControllerRef.current?.abort();
            abortControllerRef.current = new AbortController();

            const httpStream$ = runHttpRequest(url, {
                ...requestInit,
                signal: abortControllerRef.current.signal
            });

            // Parse SSE stream
            // Note: parseSSEStream returns Observable<any> where any is the JSON parsed event
            const eventStream$ = parseSSEStream(httpStream$);

            sendMessageSubscriptionRef.current = eventStream$.subscribe({
                // SSE event type is dynamically typed from server
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                next: (event: any) => {
                    console.log("Received event:", event);

                    // Handle RUN_STARTED to capture run_id
                    if (event.type === 'RunStarted' || event.type === 'RUN_STARTED') {
                        const runId = event.runId || event.run_id;
                        if (runId) {
                            setCurrentRunId(runId);
                            console.log("Run started with ID:", runId);
                        }
                    }

                    // Handle TEXT_MESSAGE_CONTENT
                    if (event.type === 'TextMessageContent' || event.type === 'TEXT_MESSAGE_CONTENT') {
                        const delta = event.delta || event.content || '';
                        if (delta) {
                            setMessages(prev => prev.map(m =>
                                m.id === assistantMessageId
                                    ? { ...m, content: m.content + delta }
                                    : m
                            ));
                        }
                    }

                    // Handle TOOL_CALL_START
                    if (event.type === 'TOOL_CALL_START' || event.type === 'ToolCallStart') {
                        const toolCallId = event.toolCallId || event.tool_call_id;
                        const toolCallName = event.toolCallName || event.tool_call_name || 'unknown';
                        const parentMessageId = event.parentMessageId || event.parent_message_id;
                        console.log(`Tool call started: ${toolCallId} (${toolCallName})`);

                        const newToolCall: ToolCall = {
                            id: toolCallId,
                            name: toolCallName,
                            arguments: {},
                            argumentsRaw: '',
                            status: 'pending' as ToolCallStatus,
                            parentMessageId,
                        };
                        toolCallsRef.current.set(toolCallId, newToolCall);
                    }

                    // Handle TOOL_CALL_ARGS
                    if (event.type === 'TOOL_CALL_ARGS' || event.type === 'ToolCallArgs') {
                        const toolCallId = event.toolCallId || event.tool_call_id;
                        const delta = event.delta || '';
                        const existing = toolCallsRef.current.get(toolCallId);
                        if (existing) {
                            existing.argumentsRaw += delta;
                            // Try to parse accumulated arguments
                            try {
                                existing.arguments = JSON.parse(existing.argumentsRaw);
                            } catch {
                                // Still accumulating, not valid JSON yet
                            }
                        }
                    }

                    // Handle TOOL_CALL_END - tool call completed (auto-calling mode)
                    if (event.type === 'TOOL_CALL_END' || event.type === 'ToolCallEnd') {
                        const toolCallId = event.toolCallId || event.tool_call_id;
                        const existing = toolCallsRef.current.get(toolCallId);
                        if (existing) {
                            existing.status = 'completed';
                            console.log(`Tool call completed: ${toolCallId}`);
                        }
                    }

                    // Handle TOOL_CALL_RESULT
                    if (event.type === 'TOOL_CALL_RESULT' || event.type === 'ToolCallResult') {
                        const toolCallId = event.toolCallId || event.tool_call_id;
                        const result = event.result;
                        const existing = toolCallsRef.current.get(toolCallId);
                        if (existing) {
                            existing.result = result;
                            console.log(`Tool call result received: ${toolCallId}`, result);
                        }
                    }

                    // Handle RUN_ERROR
                    if (event.type === 'RunError' || event.type === 'RUN_ERROR') {
                        console.error("Run Error:", event);
                        setError(`Error: ${event.message || 'Unknown error'}`);
                    }

                    // Handle RUN_FINISHED (with AG-UI Interrupts support)
                    if (event.type === 'RunFinished' || event.type === 'RUN_FINISHED') {
                        // Check for AG-UI Interrupts: outcome="interrupt"
                        if (event.outcome === 'interrupt' && event.interrupt) {
                            console.log(`RUN_FINISHED with interrupt: ${event.interrupt.id}`);
                            console.log(`Interrupt reason: ${event.interrupt.reason}`);
                            console.log(`Pending tool calls:`, event.interrupt.payload?.pendingToolCalls);

                            // Store interrupt ID for resume flow
                            setCurrentInterruptId(event.interrupt.id);

                            // Convert interrupt payload to ToolCall format
                            // eslint-disable-next-line @typescript-eslint/no-explicit-any
                            const interruptToolCalls: ToolCall[] = (event.interrupt.payload?.pendingToolCalls || []).map((tc: any) => {
                                let parsedArgs: Record<string, unknown> = {};
                                try {
                                    if (tc.fnArguments) {
                                        parsedArgs = JSON.parse(tc.fnArguments);
                                    }
                                } catch {
                                    // fnArguments is not valid JSON, keep empty object
                                }
                                return {
                                    id: tc.callId,
                                    name: tc.fnName,
                                    arguments: parsedArgs,
                                    argumentsRaw: tc.fnArguments || '',
                                    status: 'pending' as ToolCallStatus,
                                };
                            });

                            if (interruptToolCalls.length > 0) {
                                setPendingToolCalls(interruptToolCalls);
                                setIsWaitingForApproval(true);
                                // Also update ref for compatibility
                                interruptToolCalls.forEach(tc => toolCallsRef.current.set(tc.id, tc));
                            }
                        } else {
                            // Normal RUN_FINISHED (outcome="success" or legacy without outcome)
                            // Check if there are pending tool calls from ref (legacy flow)
                            const pendingCalls = Array.from(toolCallsRef.current.values())
                                .filter(tc => tc.status === 'pending');

                            if (pendingCalls.length > 0) {
                                console.log(`RUN_FINISHED: ${pendingCalls.length} pending tool calls awaiting approval (legacy)`);
                                setPendingToolCalls(pendingCalls);
                                setIsWaitingForApproval(true);
                            } else {
                                // No pending tool calls, clear the ref
                                toolCallsRef.current.clear();
                                setCurrentInterruptId(null);
                            }
                        }
                        setIsLoading(false);
                    }
                },
                error: (err: unknown) => {
                    console.error('Stream error:', err);
                    const errMessage = err instanceof Error ? err.message : 'Stream failed';
                    setError(errMessage);
                    setIsLoading(false);
                    toolCallsRef.current.clear();
                    sendMessageSubscriptionRef.current = null;
                },
                complete: () => {
                    console.log("Stream completed");
                    sendMessageSubscriptionRef.current = null;
                    // Fallback: Check for pending tool calls if RUN_FINISHED was not received
                    // This handles cases where workflow pauses for HITL without sending RUN_FINISHED
                    const pendingCalls = Array.from(toolCallsRef.current.values())
                        .filter(tc => tc.status === 'pending');

                    if (pendingCalls.length > 0) {
                        console.log(`Stream complete: ${pendingCalls.length} pending tool calls awaiting approval`);
                        setPendingToolCalls(pendingCalls);
                        setIsWaitingForApproval(true);
                        setIsLoading(false);
                    } else {
                        // No pending tool calls, clear the ref
                        toolCallsRef.current.clear();
                        setIsLoading(false);
                    }
                }
            });

        } catch (err: unknown) {
            console.error('Chat error:', err);
            const errMessage = err instanceof Error ? err.message : 'Failed to send message';
            setError(errMessage);
            setIsLoading(false);
        }
    }, [messages]);

    /**
     * Approve a tool call using AG-UI Interrupts resume flow.
     * Sends a /ag-ui/run request with resume field containing interrupt_id.
     * Server executes the tool and continues the LLM conversation.
     *
     * @param toolCallId - The ID of the tool call to approve
     * @param result - The result to return (can be modified arguments or actual result)
     */
    const approveToolCall = useCallback(async (
        toolCallId: string,
        result: unknown
    ) => {
        if (!currentInterruptId) {
            // Fallback to legacy flow if no interrupt ID
            console.warn('No interrupt ID available, approval may fail');
            setError('No interrupt ID available');
            return;
        }

        setIsLoading(true);
        setError(null);

        try {
            const baseUrl = process.env.NEXT_PUBLIC_AG_UI_BASE_URL || 'http://localhost:8080';
            const url = `${baseUrl}/ag-ui/run`;

            // Get the tool call info for display
            const toolCall = toolCallsRef.current.get(toolCallId);
            const toolName = toolCall?.name || 'unknown';
            const toolArgs = toolCall?.argumentsRaw || '{}';

            // Build runnerMessages from message history (same format as sendMessage)
            const runnerMessages = messages.filter(m => m.id !== 'welcome').map(m => ({
                role: m.role.toUpperCase(),
                content: { text: m.content }
            }));

            // AG-UI Interrupts: Send resume request
            const requestBody = {
                threadId: currentRunId || undefined,
                context: [{
                    type: 'workflow_inline',
                    data: {
                        workflow: CHAT_WORKFLOW,
                        workflowContext: JSON.stringify({
                            runnerMessages: runnerMessages
                        })
                    }
                }],
                messages: [],
                tools: [],
                resume: {
                    interruptId: currentInterruptId,
                    payload: {
                        type: 'approve',
                        toolResults: [{
                            callId: toolCallId,
                            result: result
                        }]
                    }
                }
            };

            console.log("Sending tool call approval via resume:", requestBody);

            // Cleanup previous subscription before creating new one
            approveSubscriptionRef.current?.unsubscribe();
            const approveAbortController = new AbortController();

            // Handle the SSE response stream
            const httpStream$ = runHttpRequest(url, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Accept': 'text/event-stream'
                },
                body: JSON.stringify(requestBody),
                signal: approveAbortController.signal
            });

            const eventStream$ = parseSSEStream(httpStream$);

            // Process the resumed workflow stream
            approveSubscriptionRef.current = eventStream$.subscribe({
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                next: (event: any) => {
                    console.log("Resume response event:", event);

                    // Handle TOOL_CALL_RESULT event (tool was executed)
                    if (event.type === 'TOOL_CALL_RESULT' || event.type === 'ToolCallResult') {
                        console.log("Tool call result from server:", event);
                        // Add tool result to messages for display
                        const resultMessageId = uuidv4();
                        setMessages(prev => [...prev, {
                            id: resultMessageId,
                            role: 'tool',
                            content: `Executed: ${toolName}`,
                            toolCall: {
                                name: toolName,
                                arguments: toolArgs,
                                result: event.result
                            }
                        }]);
                    }

                    // Handle TEXT_MESSAGE_START - LLM response after tool execution
                    if (event.type === 'TEXT_MESSAGE_START' || event.type === 'TextMessageStart') {
                        const messageId = event.messageId || event.message_id;
                        setMessages(prev => [...prev, {
                            id: messageId,
                            role: 'assistant',
                            content: ''
                        }]);
                    }

                    // Handle TEXT_MESSAGE_CONTENT
                    if (event.type === 'TEXT_MESSAGE_CONTENT' || event.type === 'TextMessageContent') {
                        const messageId = event.messageId || event.message_id;
                        const delta = event.delta || '';
                        setMessages(prev => prev.map(m =>
                            m.id === messageId
                                ? { ...m, content: m.content + delta }
                                : m
                        ));
                    }

                    // Handle RUN_FINISHED
                    if (event.type === 'RUN_FINISHED' || event.type === 'RunFinished') {
                        // Check for another interrupt (nested tool calls)
                        if (event.outcome === 'interrupt' && event.interrupt) {
                            console.log(`Another interrupt: ${event.interrupt.id}`);
                            setCurrentInterruptId(event.interrupt.id);
                            // eslint-disable-next-line @typescript-eslint/no-explicit-any
                            const interruptToolCalls: ToolCall[] = (event.interrupt.payload?.pendingToolCalls || []).map((tc: any) => {
                                let parsedArgs: Record<string, unknown> = {};
                                try {
                                    if (tc.fnArguments) {
                                        parsedArgs = JSON.parse(tc.fnArguments);
                                    }
                                } catch {
                                    // fnArguments is not valid JSON, keep empty object
                                }
                                return {
                                    id: tc.callId,
                                    name: tc.fnName,
                                    arguments: parsedArgs,
                                    argumentsRaw: tc.fnArguments || '',
                                    status: 'pending' as ToolCallStatus,
                                };
                            });
                            if (interruptToolCalls.length > 0) {
                                setPendingToolCalls(interruptToolCalls);
                                setIsWaitingForApproval(true);
                                interruptToolCalls.forEach(tc => toolCallsRef.current.set(tc.id, tc));
                            }
                        } else {
                            // Normal completion
                            console.log("Resume completed successfully");
                            setPendingToolCalls([]);
                            setIsWaitingForApproval(false);
                            setCurrentInterruptId(null);
                            toolCallsRef.current.clear();
                        }
                        setIsLoading(false);
                    }

                    if (event.type === 'RUN_ERROR' || event.type === 'RunError') {
                        setError(`Error: ${event.message || 'Unknown error'}`);
                        setIsLoading(false);
                    }
                },
                error: (err: unknown) => {
                    console.error('Resume stream error:', err);
                    const errMessage = err instanceof Error ? err.message : 'Resume failed';
                    setError(errMessage);
                    setIsLoading(false);
                    approveSubscriptionRef.current = null;
                },
                complete: () => {
                    console.log("Resume stream completed");
                    approveSubscriptionRef.current = null;
                }
            });

        } catch (err: unknown) {
            console.error('Tool call approval error:', err);
            const errMessage = err instanceof Error ? err.message : 'Failed to approve tool call';
            setError(errMessage);
            setIsLoading(false);
            approveSubscriptionRef.current = null;
        }
    }, [currentInterruptId, currentRunId, messages]);

    /**
     * Reject a tool call using AG-UI Interrupts resume flow.
     * Sends a /ag-ui/run request with resume payload type='reject'.
     *
     * @param toolCallId - The ID of the tool call to reject
     * @param reason - Optional reason for rejection
     */
    const rejectToolCall = useCallback(async (
        toolCallId: string,
        reason?: string
    ) => {
        if (!currentInterruptId) {
            console.warn('No interrupt ID available, rejection may fail');
            setError('No interrupt ID available');
            return;
        }

        setIsLoading(true);
        setError(null);

        try {
            const baseUrl = process.env.NEXT_PUBLIC_AG_UI_BASE_URL || 'http://localhost:8080';
            const url = `${baseUrl}/ag-ui/run`;

            // Get tool call info for logging
            const toolCall = toolCallsRef.current.get(toolCallId);
            const toolName = toolCall?.name || 'unknown';

            // AG-UI Interrupts: Send reject resume request
            const requestBody = {
                threadId: currentRunId || undefined,
                context: [{
                    type: 'workflow_inline',
                    data: {
                        workflow: CHAT_WORKFLOW,
                        workflowContext: "{}"
                    }
                }],
                messages: [],
                tools: [],
                resume: {
                    interruptId: currentInterruptId,
                    payload: {
                        type: 'reject',
                        reason: reason || 'User rejected tool call'
                    }
                }
            };

            console.log("Sending tool call rejection via resume:", requestBody);

            // Cleanup previous subscription before creating new one
            rejectSubscriptionRef.current?.unsubscribe();
            const rejectAbortController = new AbortController();

            const httpStream$ = runHttpRequest(url, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Accept': 'text/event-stream'
                },
                body: JSON.stringify(requestBody),
                signal: rejectAbortController.signal
            });

            const eventStream$ = parseSSEStream(httpStream$);

            rejectSubscriptionRef.current = eventStream$.subscribe({
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                next: (event: any) => {
                    console.log("Reject response event:", event);

                    // Handle TEXT_MESSAGE_START - LLM response after rejection
                    if (event.type === 'TEXT_MESSAGE_START' || event.type === 'TextMessageStart') {
                        const messageId = event.messageId || event.message_id;
                        setMessages(prev => [...prev, {
                            id: messageId,
                            role: 'assistant',
                            content: ''
                        }]);
                    }

                    // Handle TEXT_MESSAGE_CONTENT
                    if (event.type === 'TEXT_MESSAGE_CONTENT' || event.type === 'TextMessageContent') {
                        const messageId = event.messageId || event.message_id;
                        const delta = event.delta || '';
                        setMessages(prev => prev.map(m =>
                            m.id === messageId
                                ? { ...m, content: m.content + delta }
                                : m
                        ));
                    }

                    // Handle RUN_FINISHED
                    if (event.type === 'RUN_FINISHED' || event.type === 'RunFinished') {
                        console.log("Rejection completed");
                        // Add a system message indicating the rejection
                        setMessages(prev => [...prev, {
                            id: uuidv4(),
                            role: 'system',
                            content: `Tool call "${toolName}" was rejected${reason ? `: ${reason}` : ''}.`
                        }]);
                        setPendingToolCalls([]);
                        setIsWaitingForApproval(false);
                        setCurrentInterruptId(null);
                        toolCallsRef.current.clear();
                        setIsLoading(false);
                    }

                    if (event.type === 'RUN_ERROR' || event.type === 'RunError') {
                        setError(`Error: ${event.message || 'Unknown error'}`);
                        setIsLoading(false);
                    }
                },
                error: (err: unknown) => {
                    console.error('Reject stream error:', err);
                    const errMessage = err instanceof Error ? err.message : 'Rejection failed';
                    setError(errMessage);
                    setIsLoading(false);
                    rejectSubscriptionRef.current = null;
                },
                complete: () => {
                    console.log("Reject stream completed");
                    rejectSubscriptionRef.current = null;
                }
            });

        } catch (err: unknown) {
            console.error('Tool call rejection error:', err);
            const errMessage = err instanceof Error ? err.message : 'Failed to reject tool call';
            setError(errMessage);
            setIsLoading(false);
            rejectSubscriptionRef.current = null;
        }
    }, [currentInterruptId, currentRunId]);

    /**
     * Clear pending tool calls, interrupt state, and waiting state.
     */
    const clearPendingToolCalls = useCallback(() => {
        setPendingToolCalls([]);
        setIsWaitingForApproval(false);
        setCurrentInterruptId(null);
        toolCallsRef.current.clear();
    }, []);

    return {
        messages,
        sendMessage,
        isLoading,
        error,
        // Tool call HITL support (AG-UI Interrupts)
        pendingToolCalls,
        isWaitingForApproval,
        currentRunId,
        currentInterruptId,
        approveToolCall,
        rejectToolCall,
        clearPendingToolCalls,
    };
}
