ALTER TABLE indexes DROP CONSTRAINT IF EXISTS indexes_index_id_unique;
CREATE UNIQUE INDEX indexes_index_id_unique ON indexes USING btree (index_id varchar_pattern_ops);
