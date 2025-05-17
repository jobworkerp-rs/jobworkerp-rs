
-- function set definition
DROP TABLE IF EXISTS function_set;
CREATE TABLE `function_set` (
  `id` BIGINT(10) PRIMARY KEY,
  `name` VARCHAR(128) NOT NULL, -- name for identification
  `description` TEXT NOT NULL, -- function set description
  `category` INT(10) NOT NULL DEFAULT 0, -- function set category (optional)
  UNIQUE KEY `name` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- function set target definition
DROP TABLE IF EXISTS function_set_target;
CREATE TABLE `function_set_target` (
  `id` BIGINT(10) PRIMARY KEY AUTO_INCREMENT,
  `set_id` BIGINT(10) NOT NULL, -- function set id
  `target_id` BIGINT(10) NOT NULL, -- function set target id(worker or runner)
  `target_type` INT(10) NOT NULL DEFAULT 0, -- function set target type (runner: 0 or worker: 1)
  UNIQUE KEY `set_target` (`set_id`, `target_id`, `target_type`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

