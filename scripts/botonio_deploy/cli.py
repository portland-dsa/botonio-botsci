import che_deploya
from che_deploya import (
    DeploySpec,
    Component,
    StaticUnit,
    TemplatedUnit,
    Secret,
    Environment,
    Stages,
    FilePermissions,
)
from che_deploya import db
from che_deploya.db import Db, Privilege, On, Grant, Role


from enum import StrEnum

DB_MIGRATION_PASSWORD = "db_migration_password"


class BotSecrets(StrEnum):
    DiscordBotToken = "discord_bot_token"
    SolidarityTechToken = "solidarity_tech_token"
    DbMigrationPassword = DB_MIGRATION_PASSWORD
    AuditHashKey = "audit_hash_key"
    SsoOauthClientSecret = "sso_oauth_client_secret"
    SsoSigningKey = "sso_signing_key"
    SsoCallerBearer = "sso_caller_bearer"


class BotVars(StrEnum):
    DiscordGuildId = "discord_guild_id"
    StUserListId = "st_user_list_id"
    # Discord: Personas (staging mock only)
    GoodStandingUserId = "good_standing_id"
    ExpiringUserId = "expiring_standing_id"
    LapsedUserId = "lapsed_user_id"

    # SSO role-check (non-secret per-env config): the OAuth client id is public and the redirect
    # uri is the relay's callback, so they render in via substitution like the values above
    # rather than being hand-edited into the rendered unit.
    SsoOauthClientId = "sso_oauth_client_id"
    SsoRedirectUri = "sso_redirect_uri"


class BackupVars(StrEnum):
    B2BucketName = "b2_bucket_name"
    B2KeyId = "b2_key_id"
    BackupAgeRecipient = "box_age_pubkey"


class BackupSecrets(StrEnum):
    B2ApplicationKey = "b2_application_key"


class DbSecrets(StrEnum):
    DbMigrationPassword = DB_MIGRATION_PASSWORD


SPEC = DeploySpec(
    root="botonio-botsci",
    package="botonio_deploy",
    components=[
        Component(
            name="bot",
            secrets=Secret(
                names=frozenset(BotSecrets),
                src="{repo_root}/secrets/{stage}.enc.yaml",
                exclude={Stages.Staging: {BotSecrets.SolidarityTechToken}},
            ),
            units=[
                TemplatedUnit(
                    src="{repo_root}/deploy/systemd/botonio-botsci@{stage}.service.d/override.conf.tmpl",
                    resource_loc="assets/botonio-botsci@{stage}.service.d/override.conf.tmpl",
                    dest="/etc/systemd/system/botonio-botsci@{stage}.service.d/override.conf",
                    per_stage=True,
                    env=Environment(
                        names=frozenset(BotVars),
                        exclude={
                            Stages.Production: {
                                BotVars.ExpiringUserId,
                                BotVars.GoodStandingUserId,
                                BotVars.LapsedUserId,
                            }
                        },
                    ),
                ),
                StaticUnit(
                    src="{repo_root}/deploy/systemd/botonio-botsci@.service",
                    dest="/etc/systemd/system/botonio-botsci@.service",
                ),
            ],
            db=Db(
                group_role=Role(
                    name="botonio_app",
                ),
                roles=[
                    Role(
                        name="botonio_{stage}_migrate",
                        login=True,
                        password=DbSecrets.DbMigrationPassword,
                    ),
                    Role(
                        name="botonio_{stage}_app", login=True, member_of="botonio_app"
                    ),
                ],
                databases=[
                    db.Database(name="botonio_{stage}", owner="botonio_{stage}_migrate")
                ],
                # Careful, order sensitive!
                revokes=[
                    db.Revoke(privileges={db.Privilege.All}, on=db.On.schema("public"))
                ],
                grants=[
                    Grant(
                        privileges={Privilege.Connect},
                        on=On.database("botonio_{stage}"),
                        to="botonio_{stage}_app",
                    ),
                    Grant(
                        privileges={Privilege.Usage},
                        on=On.schema("public"),
                        to="botonio_{stage}_app",
                    ),
                    Grant(
                        privileges={Privilege.All},
                        on=On.schema("public"),
                        to="botonio_{stage}_migrate",
                    ),
                    Grant(
                        privileges={Privilege.Delete},
                        on=On.table("manual_override"),
                        to="botonio_app",
                        only={Stages.Staging},
                        require_exists=True,
                    ),
                ],
            ),
        ),
        Component(
            name="backup",
            stages={Stages.Production},
            secrets=Secret(
                names=frozenset(BackupSecrets),
                src="{repo_root}/secrets/{stage}.enc.yaml",
            ),
            units=[
                TemplatedUnit(
                    src="{repo_root}/deploy/systemd/botonio-db-backup.service.tmpl",
                    dest="/etc/systemd/system/botonio-db-backup.service",
                    env=Environment(names=frozenset(BackupVars)),
                ),
                StaticUnit(
                    src="{repo_root}/deploy/systemd/botonio-db-backup.timer",
                    dest="/etc/systemd/system/botonio-db-backup.timer",
                ),
                StaticUnit(
                    src="{repo_root}/deploy/systemd/botonio-db-backup.py",
                    dest="/usr/local/sbin/botonio-db-backup",
                    # This is the default but also like what if we didn't clobber sbin's permissions
                    dir_mode=FilePermissions.WorldDir,
                    file_mode=FilePermissions.PrivateExecFile,
                ),
            ],
        ),
    ],
)

main = che_deploya.build_cli(SPEC)
