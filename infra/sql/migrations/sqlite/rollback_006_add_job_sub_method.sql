-- Rollback sub_method column addition from job table
-- SQLite does not support DROP COLUMN directly in older versions

-- For SQLite 3.35.0+ (2021-03-12), this works:
ALTER TABLE `job` DROP COLUMN `sub_method`;
