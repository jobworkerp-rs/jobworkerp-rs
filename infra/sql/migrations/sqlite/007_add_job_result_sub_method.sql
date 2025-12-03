-- Add using column to job_result table for Runner Using feature
-- This allows preserving using for retry and periodic job re-execution

ALTER TABLE `job_result` ADD COLUMN `using` TEXT;
