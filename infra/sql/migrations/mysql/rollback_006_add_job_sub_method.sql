-- Rollback sub_method column addition from job table

ALTER TABLE `job` DROP COLUMN `sub_method`;
