-- Rollback: Remove using column from job_result table

ALTER TABLE `job_result` DROP COLUMN `using`;
