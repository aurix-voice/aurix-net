-- Soft delete: the row stays so usage_hours and audit keep their project reference.
ALTER TABLE projects ADD COLUMN deleted_at TIMESTAMPTZ;
