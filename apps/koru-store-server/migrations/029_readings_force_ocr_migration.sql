-- readings.db version 2 -> 3. Preserve existing readings, failures and requests.
-- 0 = ordinary read, 1 = force OCR from page 1, 2 = force OCR continuing progress.
BEGIN IMMEDIATE;
ALTER TABLE read_requests ADD COLUMN force_ocr INTEGER NOT NULL DEFAULT 0
    CHECK (force_ocr IN (0, 1, 2));
PRAGMA user_version = 3;
COMMIT;
