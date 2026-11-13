-- Team suspension (spec 2026-11-13-okf-team-suspension-design.md). A
-- suspended team's catalogs stop answering non-admin credentials until the
-- suspension is lifted; nothing is deleted. The notice is shown to members.
ALTER TABLE teams
    ADD COLUMN suspended_at timestamptz,
    ADD COLUMN suspension_notice text
        CHECK (char_length(suspension_notice) <= 2000),
    ADD CONSTRAINT teams_notice_needs_suspension
        CHECK (suspension_notice IS NULL OR suspended_at IS NOT NULL);
