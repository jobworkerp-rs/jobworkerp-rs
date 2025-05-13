
-- function set definition
CREATE TABLE `function_set` (
  `id` BIGINT PRIMARY KEY,
  `name` TEXT NOT NULL, -- name for identification
  `description` TEXT NOT NULL, -- function set description
  `category` INT NOT NULL DEFAULT 0 -- function set category (optional)
);
CREATE UNIQUE INDEX IF NOT EXISTS `name` ON function_set(`name`);

-- function set target definition
CREATE TABLE `function_set_target` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `set_id` BIGINT NOT NULL, -- function set id
  `target_id` BIGINT NOT NULL, -- function set target id(worker or runner)
  `target_type` INTEGER NOT NULL DEFAULT 0 -- function set target type (runner: 0 or worker: 1)
);

CREATE UNIQUE INDEX IF NOT EXISTS `set_target` ON function_set_target(`set_id`, `target_id`, `target_type`);
