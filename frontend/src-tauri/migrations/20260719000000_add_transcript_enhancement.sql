ALTER TABLE transcripts ADD COLUMN enhanced_transcript TEXT;

ALTER TABLE meetings ADD COLUMN transcript_enhancement_status TEXT NOT NULL DEFAULT 'idle';
ALTER TABLE meetings ADD COLUMN transcript_enhancement_error TEXT;
