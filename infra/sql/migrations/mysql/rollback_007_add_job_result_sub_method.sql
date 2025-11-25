-- Rollback: Remove sub_method column from job_result table

ALTER TABLE `job_result` DROP COLUMN `sub_method`;
