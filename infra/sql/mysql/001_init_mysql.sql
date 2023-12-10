-- fixed test db
-- CREATE DATABASE IF NOT EXISTS `jobworkerp_test`;
-- GRANT ALL ON jobworkerp_test.* TO user IDENTIFIED BY 'user_pw' WITH GRANT OPTION;

-- USE `jobworkerp_test`;

-- now using fixed named 'test' db (from docker-compose definition)
USE `test`;

-- to ignore sqlx db migration
DROP TABLE IF EXISTS  _sqlx_migration;

-- FLUSH PRIVILEGES;

-- SET time_zone = '+09:00';
