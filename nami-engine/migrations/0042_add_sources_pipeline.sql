-- Per-source ingest pipeline (Logs part 2): an ordered list of at most 50
-- rules, replaced as a whole by PUT .../pipeline and validated by the engine
-- (logs::pipeline::compile) before it is stored. The engine caches compiled
-- rules per source keyed on sources.updated_at, so every write here must bump
-- updated_at (models::source::set_pipeline does).
alter table sources
    add column pipeline jsonb not null default '[]'::jsonb
        check (jsonb_typeof(pipeline) = 'array' and jsonb_array_length(pipeline) <= 50);
