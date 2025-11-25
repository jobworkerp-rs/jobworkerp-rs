-- Add sub_method column to job table for Runner Sub-Method feature
-- This allows specifying which tool/sub-method to use when executing MCP/Plugin runners

ALTER TABLE `job` ADD COLUMN `sub_method` TEXT;
