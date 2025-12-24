"use client";

import { useState, useRef, useEffect } from "react";
import { useAgUiChat } from "@/hooks/useAgUiChat";
import { ToolCallApproval } from "@/components/ToolCallApproval";

export default function Home() {
  const [input, setInput] = useState("");
  const {
    messages,
    sendMessage,
    isLoading,
    error,
    pendingToolCalls,
    isWaitingForApproval,
    approveToolCall,
    rejectToolCall,
  } = useAgUiChat();
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim() || isLoading) return;
    
    const msg = input;
    setInput("");
    await sendMessage(msg);
  };

  return (
    <div className="flex h-screen bg-slate-900 text-slate-100 font-sans">
      {/* Sidebar / Info */}
      <div className="w-80 bg-slate-800 border-r border-slate-700 p-6 hidden md:flex flex-col">
        <h1 className="text-2xl font-bold bg-gradient-to-r from-blue-400 to-emerald-400 text-transparent bg-clip-text mb-6">
          AG-UI Client
        </h1>
        
        <div className="space-y-6">
          <div className="bg-slate-700/50 p-4 rounded-lg border border-slate-600">
            <h2 className="font-semibold text-blue-300 mb-2">Status</h2>
            <div className="flex items-center gap-2">
              <span className={`w-2 h-2 rounded-full ${
                isWaitingForApproval
                  ? 'bg-orange-400 animate-pulse'
                  : isLoading
                  ? 'bg-yellow-400 animate-pulse'
                  : 'bg-emerald-400'
              }`}></span>
              <span className="text-sm text-slate-300">
                {isWaitingForApproval
                  ? 'Awaiting Approval'
                  : isLoading
                  ? 'Processing...'
                  : 'Ready'}
              </span>
            </div>
            {isWaitingForApproval && (
              <div className="mt-2 text-xs text-orange-400 bg-orange-900/20 p-2 rounded">
                {pendingToolCalls.length} tool call(s) awaiting your approval
              </div>
            )}
            {error && (
                <div className="mt-2 text-xs text-red-400 bg-red-900/20 p-2 rounded">
                    Error: {error}
                </div>
            )}
          </div>

          <div className="text-sm text-slate-400">
            <h3 className="font-semibold text-slate-300 mb-2">Instructions</h3>
            <ul className="list-disc pl-4 space-y-1">
                <li>Connected to AG-UI Front Server</li>
                <li>Uses LLM_CHAT runner</li>
                <li>Inline workflow definition</li>
            </ul>
          </div>
        </div>
      </div>

      {/* Main Chat Area */}
      <div className="flex-1 flex flex-col max-w-4xl mx-auto w-full">
        {/* Header (Mobile only) */}
        <div className="md:hidden p-4 bg-slate-800 border-b border-slate-700">
            <h1 className="text-lg font-bold">AG-UI Client</h1>
        </div>

        {/* Messages */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {messages.map((m) => (
            <div
              key={m.id}
              className={`flex ${
                m.role === 'user' ? 'justify-end' :
                m.role === 'tool' ? 'justify-center' :
                'justify-start'
              }`}
            >
              {m.role === 'tool' ? (
                // Tool execution result - centered with distinct styling
                <div className="max-w-[90%] p-3 rounded-lg bg-emerald-900/30 border border-emerald-700/50">
                  <div className="flex items-center gap-2 text-xs text-emerald-400 mb-2">
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="w-4 h-4">
                      <path fillRule="evenodd" d="M16.704 4.153a.75.75 0 01.143 1.052l-8 10.5a.75.75 0 01-1.127.075l-4.5-4.5a.75.75 0 011.06-1.06l3.894 3.893 7.48-9.817a.75.75 0 011.05-.143z" clipRule="evenodd" />
                    </svg>
                    <span className="font-semibold">Tool Executed</span>
                  </div>
                  <div className="text-sm text-slate-300 font-mono bg-slate-800/50 p-2 rounded">
                    <div className="text-emerald-300">{m.toolCall?.name?.replace(/___/g, '/')}</div>
                    {m.toolCall?.arguments && (
                      <div className="text-xs text-slate-400 mt-1">
                        Args: <code className="text-slate-300">{m.toolCall.arguments}</code>
                      </div>
                    )}
                    {m.toolCall?.result && (
                      <div className="text-xs text-slate-400 mt-1">
                        Result: <code className="text-emerald-200">{JSON.stringify(m.toolCall.result)}</code>
                      </div>
                    )}
                  </div>
                </div>
              ) : (
                // Regular user/assistant message
                <div
                  className={`max-w-[80%] p-4 rounded-2xl ${
                    m.role === 'user'
                      ? 'bg-blue-600 text-white rounded-br-none'
                      : 'bg-slate-700 text-slate-100 rounded-bl-none border border-slate-600'
                  }`}
                >
                  <div className="text-xs opacity-50 mb-1 capitalize">{m.role}</div>
                  <div className="whitespace-pre-wrap">{m.content}</div>
                </div>
              )}
            </div>
          ))}

          {/* Tool Call Approval UI */}
          {isWaitingForApproval && pendingToolCalls.length > 0 && (
            <ToolCallApproval
              toolCalls={pendingToolCalls}
              isLoading={isLoading}
              onApprove={approveToolCall}
              onReject={rejectToolCall}
            />
          )}

          <div ref={messagesEndRef} />
        </div>

        {/* Input Area */}
        <div className="p-4 bg-slate-900 border-t border-slate-800">
          <form onSubmit={handleSubmit} className="flex gap-2">
            <input
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder={isWaitingForApproval ? "Please approve or reject pending tool calls first..." : "Type your message..."}
              className="flex-1 bg-slate-800 text-white p-3 rounded-xl border border-slate-700 focus:border-blue-500 focus:ring-1 focus:ring-blue-500 outline-none transition"
              disabled={isLoading || isWaitingForApproval}
            />
            <button
              type="submit"
              disabled={isLoading || isWaitingForApproval || !input.trim()}
              className="bg-blue-600 hover:bg-blue-500 disabled:bg-slate-700 disabled:cursor-not-allowed text-white px-6 py-3 rounded-xl font-semibold transition flex items-center gap-2"
            >
              <span>Send</span>
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="w-5 h-5">
                <path d="M3.105 2.289a.75.75 0 00-.826.95l1.414 4.925A1.5 1.5 0 005.135 9.25h6.115a.75.75 0 010 1.5H5.135a1.5 1.5 0 00-1.442 1.086l-1.414 4.926a.75.75 0 00.826.95 28.896 28.896 0 0015.293-7.154.75.75 0 000-1.115A28.897 28.897 0 003.105 2.289z" />
              </svg>
            </button>
          </form>
          <div className="text-center mt-2 text-xs text-slate-500">
            Powered by @ag-ui/client
          </div>
        </div>
      </div>
    </div>
  );
}
