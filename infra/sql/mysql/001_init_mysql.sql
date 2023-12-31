-- fixed test db

-- now using fixed named 'test' db (from docker-compose definition)
CREATE DATABASE IF NOT EXISTS `test`;
USE `test`;
-- GRANT ALL ON test.* TO mysql IDENTIFIED BY 'mysql' WITH GRANT OPTION;
-- FLUSH PRIVILEGES;

-- to ignore sqlx db migration
DROP TABLE IF EXISTS  _sqlx_migration;


-- SET time_zone = '+09:00';
