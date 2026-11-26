-- GORM v1.23.10 + gorm.io/driver/sqlite v1.3.6 schema.
CREATE TABLE `users` (`id` integer,`created_at` datetime,`updated_at` datetime,PRIMARY KEY (`id`));
CREATE TABLE `sources` (`id` integer,`link` text,`title` text,`error_count` integer,`created_at` datetime,`updated_at` datetime,PRIMARY KEY (`id`));
CREATE TABLE `contents` (`source_id` integer,`hash_id` text,`raw_id` text,`raw_link` text,`title` text,`telegraph_url` text,`created_at` datetime,`updated_at` datetime,PRIMARY KEY (`hash_id`),CONSTRAINT `fk_sources_content` FOREIGN KEY (`source_id`) REFERENCES `sources`(`id`));
CREATE TABLE `subscribes` (`id` integer,`user_id` integer,`source_id` integer,`enable_notification` integer,`enable_telegraph` integer,`tag` text,`interval` integer,`wait_time` integer,`created_at` datetime,`updated_at` datetime,PRIMARY KEY (`id`));

INSERT INTO `users` VALUES (-1009876543210,'2025-01-01 08:00:00.123+08:00','2025-01-01 08:00:00.123+08:00');
INSERT INTO `sources` VALUES (20,'https://example.org/old','Old feed',NULL,'2025-01-01 08:00:00.123+08:00','2025-01-01 08:00:00.123+08:00');
INSERT INTO `sources` VALUES (42,'https://example.org/feed','Existing feed',2,'2025-01-02 08:00:00.123+08:00','2025-01-02 08:00:00.123+08:00');
INSERT INTO `contents` VALUES (42,'c37ebe96','legacy-id','https://example.org/item','Existing article','https://telegra.ph/item','2025-01-02 08:00:00.123+08:00','2025-01-02 08:00:00.123+08:00');
INSERT INTO `subscribes` VALUES (9,-1009876543210,20,1,0,'old',5,4,'2025-01-01 08:00:00.123+08:00','2025-01-01 08:00:00.123+08:00');
INSERT INTO `subscribes` VALUES (12,-1009876543210,42,0,1,'news',11,3,'2025-01-02 08:00:00.123+08:00','2025-01-02 08:00:00.123+08:00');
INSERT INTO `subscribes` VALUES (13,123456,42,1,0,NULL,10,9,'2025-01-02 08:00:00.456+08:00','2025-01-02 08:00:00.456+08:00');
