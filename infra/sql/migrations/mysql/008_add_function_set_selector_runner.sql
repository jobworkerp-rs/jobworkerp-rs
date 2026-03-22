-- Add FUNCTION_SET_SELECTOR builtin runner
INSERT IGNORE INTO runner (id, name, description, definition, type) VALUES (
  8, 'FUNCTION_SET_SELECTOR',
  'Lists available FunctionSets with tool summaries for LLM tool selection. Used as a meta-tool to help LLM discover and select appropriate FunctionSets.',
  'builtin8', 8
);
