-- Add using column to job table for Runner Using feature
-- This allows specifying which tool/implementation to use when executing MCP/Plugin runners

ALTER TABLE `job` ADD COLUMN `using` VARCHAR(255) NULL;
