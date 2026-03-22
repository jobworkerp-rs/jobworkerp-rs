-- Rollback: Remove FUNCTION_SET_SELECTOR builtin runner
DELETE FROM runner WHERE id = 8 AND type = 8;
