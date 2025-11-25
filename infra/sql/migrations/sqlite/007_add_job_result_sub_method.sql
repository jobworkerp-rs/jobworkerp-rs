-- Add sub_method column to job_result table for Runner Sub-Method feature
-- This allows preserving sub_method for retry and periodic job re-execution

ALTER TABLE `job_result` ADD COLUMN `sub_method` TEXT;
