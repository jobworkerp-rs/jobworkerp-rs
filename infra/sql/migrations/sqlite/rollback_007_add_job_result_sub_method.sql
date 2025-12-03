-- Rollback: Remove using column from job_result table
-- Note: SQLite does not support DROP COLUMN directly in older versions
-- For SQLite 3.35.0+, use:
ALTER TABLE `job_result` DROP COLUMN `using`;

-- For older SQLite versions, you would need to recreate the table without the column
