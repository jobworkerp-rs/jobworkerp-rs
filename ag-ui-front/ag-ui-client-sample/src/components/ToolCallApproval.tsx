'use client';

import React, { useState } from 'react';
import { ToolCall } from '../types/tool';

interface ToolCallApprovalProps {
    /** List of pending tool calls to display */
    toolCalls: ToolCall[];
    /** Whether the component is in loading state */
    isLoading?: boolean;
    /** Callback when a tool call is approved */
    onApprove: (toolCallId: string, result: unknown) => void;
    /** Callback when a tool call is rejected */
    onReject: (toolCallId: string, reason?: string) => void;
}

/**
 * Component for displaying and approving/rejecting LLM tool calls.
 * Shows each pending tool call as a card with the ability to edit arguments
 * and approve or reject the call.
 */
export function ToolCallApproval({
    toolCalls,
    isLoading = false,
    onApprove,
    onReject,
}: ToolCallApprovalProps) {
    // Track edited arguments for each tool call
    const [editedArgs, setEditedArgs] = useState<Record<string, string>>({});
    // Track rejection reasons
    const [rejectReasons, setRejectReasons] = useState<Record<string, string>>({});

    if (toolCalls.length === 0) {
        return null;
    }

    const handleArgsChange = (toolCallId: string, value: string) => {
        setEditedArgs(prev => ({ ...prev, [toolCallId]: value }));
    };

    const handleReasonChange = (toolCallId: string, value: string) => {
        setRejectReasons(prev => ({ ...prev, [toolCallId]: value }));
    };

    const handleApprove = (toolCall: ToolCall) => {
        const argsText = editedArgs[toolCall.id] || toolCall.argumentsRaw;
        let result: unknown;
        try {
            result = JSON.parse(argsText);
        } catch {
            result = argsText;
        }
        onApprove(toolCall.id, result);
    };

    const handleReject = (toolCall: ToolCall) => {
        const reason = rejectReasons[toolCall.id];
        onReject(toolCall.id, reason);
    };

    const getStatusBadgeClass = (status: string) => {
        switch (status) {
            case 'pending':
                return 'bg-yellow-500/20 text-yellow-300 border border-yellow-500/30';
            case 'approved':
                return 'bg-green-500/20 text-green-300 border border-green-500/30';
            case 'rejected':
                return 'bg-red-500/20 text-red-300 border border-red-500/30';
            case 'executing':
                return 'bg-blue-500/20 text-blue-300 border border-blue-500/30';
            case 'completed':
                return 'bg-cyan-500/20 text-cyan-300 border border-cyan-500/30';
            default:
                return 'bg-slate-500/20 text-slate-300 border border-slate-500/30';
        }
    };

    return (
        <div className="bg-slate-800 border border-orange-500/50 rounded-xl p-4 my-4">
            {/* Header */}
            <div className="mb-4">
                <h3 className="text-lg font-semibold text-orange-400 flex items-center gap-2">
                    <svg xmlns="http://www.w3.org/2000/svg" className="h-5 w-5" viewBox="0 0 20 20" fill="currentColor">
                        <path fillRule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-7-4a1 1 0 11-2 0 1 1 0 012 0zM9 9a1 1 0 000 2v3a1 1 0 001 1h1a1 1 0 100-2v-3a1 1 0 00-1-1H9z" clipRule="evenodd" />
                    </svg>
                    Tool Call Approval Required
                </h3>
                <p className="text-sm text-slate-400 mt-1">
                    The AI wants to execute the following tool calls. Please review and approve or reject each one.
                </p>
            </div>

            {/* Tool Call List */}
            <div className="space-y-3">
                {toolCalls.map(toolCall => (
                    <div
                        key={toolCall.id}
                        className="bg-slate-700/50 border border-slate-600 rounded-lg p-3"
                    >
                        {/* Card Header */}
                        <div className="flex justify-between items-center mb-2">
                            <span className="font-semibold text-emerald-400 text-base">
                                {toolCall.name}
                            </span>
                            <span className={`px-2 py-0.5 rounded text-xs uppercase font-medium ${getStatusBadgeClass(toolCall.status)}`}>
                                {toolCall.status}
                            </span>
                        </div>

                        {/* Tool Call ID */}
                        <div className="text-xs text-slate-500 font-mono mb-2">
                            ID: {toolCall.id}
                        </div>

                        {/* Arguments */}
                        <div className="mb-3">
                            <label
                                htmlFor={`args-${toolCall.id}`}
                                className="block text-xs text-slate-400 mb-1"
                            >
                                Arguments:
                            </label>
                            <textarea
                                id={`args-${toolCall.id}`}
                                value={editedArgs[toolCall.id] ?? toolCall.argumentsRaw}
                                onChange={(e) => handleArgsChange(toolCall.id, e.target.value)}
                                disabled={toolCall.status !== 'pending' || isLoading}
                                className="w-full font-mono text-sm p-2 bg-slate-900 text-slate-200 border border-slate-600 rounded resize-y focus:border-blue-500 focus:ring-1 focus:ring-blue-500 outline-none disabled:bg-slate-800 disabled:text-slate-500"
                                rows={3}
                            />
                        </div>

                        {/* Action Buttons */}
                        {toolCall.status === 'pending' && (
                            <div className="flex gap-2 mb-2">
                                <button
                                    onClick={() => handleApprove(toolCall)}
                                    disabled={isLoading}
                                    className="flex-1 px-4 py-2 bg-green-600 hover:bg-green-500 disabled:bg-slate-600 disabled:cursor-not-allowed text-white font-medium rounded transition-colors"
                                >
                                    {isLoading ? 'Processing...' : 'Approve'}
                                </button>
                                <button
                                    onClick={() => handleReject(toolCall)}
                                    disabled={isLoading}
                                    className="flex-1 px-4 py-2 bg-red-600 hover:bg-red-500 disabled:bg-slate-600 disabled:cursor-not-allowed text-white font-medium rounded transition-colors"
                                >
                                    Reject
                                </button>
                            </div>
                        )}

                        {/* Rejection Reason Input */}
                        {toolCall.status === 'pending' && (
                            <div className="mt-2">
                                <input
                                    type="text"
                                    placeholder="Rejection reason (optional)"
                                    value={rejectReasons[toolCall.id] || ''}
                                    onChange={(e) => handleReasonChange(toolCall.id, e.target.value)}
                                    disabled={isLoading}
                                    className="w-full px-3 py-2 bg-slate-900 text-slate-200 border border-slate-600 rounded text-sm focus:border-blue-500 focus:ring-1 focus:ring-blue-500 outline-none placeholder:text-slate-500"
                                />
                            </div>
                        )}

                        {/* Result Display */}
                        {toolCall.result !== undefined && (
                            <div className="mt-3 p-2 bg-slate-900/50 rounded border border-slate-600">
                                <label className="block text-xs text-slate-400 mb-1">Result:</label>
                                <pre className="text-xs text-slate-300 overflow-x-auto">
                                    {JSON.stringify(toolCall.result, null, 2)}
                                </pre>
                            </div>
                        )}
                    </div>
                ))}
            </div>
        </div>
    );
}
