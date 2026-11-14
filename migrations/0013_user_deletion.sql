-- Deleting a user (spec Part 3): the account and every credential it ever
-- had go in one transaction; catalogs and their history stay. The code's
-- three refusals (a personal team that owns catalogs, the last owner of a
-- shared team, the last admin user) replace the database's RESTRICT.
-- teams.personal_of stays RESTRICT: deletion removes the personal team
-- itself, after checking it owns nothing.
--
-- No IF EXISTS: a database whose constraints are named otherwise should
-- stop here rather than keep RESTRICT silently. Deleting a key cascades its
-- key_catalogs and oauth_tokens; changesets.key_id, maintenance_runs.key_id
-- and sign_in_codes.grant_id become NULL (users::delete discards the open
-- changesets first).
ALTER TABLE api_keys
    DROP CONSTRAINT api_keys_user_id_fkey,
    ADD CONSTRAINT api_keys_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE sign_in_codes
    DROP CONSTRAINT sign_in_codes_user_id_fkey,
    ADD CONSTRAINT sign_in_codes_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE team_members
    DROP CONSTRAINT team_members_user_id_fkey,
    ADD CONSTRAINT team_members_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
