-- fixed test db

-- now using fixed named 'test' db (from docker-compose definition)
CREATE DATABASE IF NOT EXISTS `test`;
USE `test`;
-- GRANT ALL ON test.* TO mysql IDENTIFIED BY 'mysql' WITH GRANT OPTION;
-- FLUSH PRIVILEGES;

-- Note: Do not drop or modify _sqlx_migrations table here
-- It is managed by sqlx migrator and will be created/updated automatically


-- SET time_zone = '+09:00';
