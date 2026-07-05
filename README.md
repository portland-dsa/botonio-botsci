# Botonio Botsci

Say hello to Botonio Botsci, the Portland DSA Discord bot.

This bot has two main purposes:
1. Automatically verify people who join Discord members with our records
2. Terraform a large, already active Discord server to minimize disruption for implementing (1)

There are future planned features, like adding people to Google Drive via Discord command (see the [Google Workspace Project](https://github.com/portland-dsa/workspace-sync)). It also acts as an SSO provider for that project, validating users.

This runs on a hardened remote machine in a systemd unit. Deployment instructions forthcoming.

## Running

First, [install Rust](https://rustup.rs). Then, you'll need to gather a few credentials into Environment Variables:
- A Solidarity Tech API Key [`SOLIDARITY_TECH_TOKEN`]
- A Discord Bot Token [`DISCORD_BOT_TOKEN`]
- A Discord Guide (server) ID, for the Server you want the bot to manage. [`DISCORD_GUILD_ID`]
- A Discord Role ID, for users that are considered moderators (only they have access to certain commands).

Note: make sure you've *invited* the Bot to the Discord server first, and that it has admin permissions and the `GUILD_MEMBERS` intent. (Note to self: fill this out later). Otherwise this will crash with an obnoxious unclear error message. You may have to enable Discord developer mode to copy the IDs.

Put those in the file `.env` like
```
DISCORD_BOT_TOKEN=<your token>
[...]
```

Then, after reloading your terminal, run:
```
cargo run --bin botonio-botsci
```

### Development

To test, you'll need a few added environment variables:

```
ST_LIVE_EMAIL="a-real-email@example.com" 
ST_LIVE_ALLOW_NOOP_WRITE=1 # Allows the tests to write to Solidarity Tech, all test make no visible changes
SOLIDARITY_TECH_DISCORD_LIST_ID= # A list that contains only members who have a discord handle or user ID set
DISCORD_TEST_USER_ID= # The ID of a specific user in your test server to be a role modification guinea pig
DISCORD_TEST_CHANNEL_ID= # A channel ID to run role permission tests on
```

Then you can run:
```
cargo test --all-features --all-targets --test discord_live --test solidarity_tech_live --test solidarity_tech_mock
```

To run every test. You should see a bunch of Sonic characters doing things.

**Note** one unfortunate thing is you can't really have "throwaway" Solidarity Tech instances very easily, so this will probably be your live key to your live server that has real people. I recommend commenting out the Solidarity Tech token value when developing in case you make a mistake.

### Running for SSO testing

This setup is a little more involved than other setup methods and involves a few steps. Firstly, you **must** be on Linux or WSL and [install Podman](https://podman.io/docs/installation). Special note about Podman on WSL - don't do the `podman machine init` instructions, use [these](https://podman-desktop.io/docs/podman/accessing-podman-from-another-wsl-instance). It's running *inside* another hypervisor, so you can't use the normal instructions without some weird cgroup and nested virtualization setup if you run the normal instructions for Linux/Windows.

Next you must enable the Discord Developer Tools in the settings.

#### 1. Setting up the Podman container

You need to set up the database before building because of `sqlx` cache checks on build. This isn't *strictly* necessary with `SQLX_OFFLINE`, but the container needs to be running *anyway* later down the line, so you may as well get it up. Do this *in WSL or Linux*:

```zsh
podman compose -f ./deploy/test-infra/compose.yaml up -d --force-recreate
cargo install sqlx-cli
cargo sqlx migrate run --source crates/persistence/migrations/
```

#### 2. Setting up a Discord test environment

You'll need to make your own Discord server, and Discord bot. Making a Discord server should be easy to look up or do on your own.

First, go to https://discord.com/developers/applications and make a developer account if you haven't.

Then, click `New Application`. Name it what you want, but something like `Botonio Testing` or whatever is descriptive. **Note**: The order of the following steps is important. However, the only real thing is you need `Install Link` off or Discord chokes on certain things.

Then enable the following settings:
`Bot` -> `Server Members Intent` (true)
`Installation` -> `Install Link` -> `None`
`Installation` -> `Installation Contexts` -> `Guild Install` -> Checked; `User Install` -> Unchecked

Store the following:
- `BOT_SSO_REDIRECT_URI`
  - `OAuth2` -> `Redirects` -> Something like `http://localhost:9999/api/auth/callback` # This can be *any* port so long as it ends in `/api/auth/callback`
- `BOT_SSO_OAUTH_CLIENT_ID` and `BOT_SSO_OAUTH_CLIENT_SECRET`
  - `OAuth2` -> `Client Information` -> `Reset Secret` -> Copy from the text boxes (make sure `Public client` is unchecked)
- `DISCORD_BOT_TOKEN`
  - `Bot` -> `Reset Token` -> Copy that value

Now you need to *invite your bot to the Server* and perform the following server setup:

Create three channels (this is less than a real setup but you can reuse a couple):
- `dues-expired`
- `unverified-members`
- `mod-verification`

Create six roles:
- Member
- Unverified
- Dues Expired
- Dues Expiring
- Manually Verified
- Discord Admin [*Make sure this has the `Administrator` privilege enabled and is *dragged to the top of the role list*]

These names are optional, they're just the fastest to quick start. Make sure to assign yourself the `Discord Admin` role!

Now invite your bot, from the developer portal:

`OAuth2` -> `OAuth2 URL Generator` -> check `bot` -> a panel will pop up called `Bot Permissions`, on that check `Administrator` -> Integration Type -> `Guild Install` -> Copy the generated URL, paste it in your browser, then accept the invite

**Important**: Go *back* into the rolls and *drag the role named after the bot to the top, under `Discord Admin`.

Phew... that was tough. We have a little more to do on Discord before we get it running. Luckily, you only have to do this part once!

#### 3. Information to gather before we start

`DISCORD_GUILD_ID`: right click your testing server -> Copy server info -> Copy server ID
Your *user ID*: right click yourself in any server -> Copy User ID

##### 3.1 Minting testing Keys

You'll need an HMAC audit hashkey. Generate the following keys and keep them for the next step:

On any machine with `openssl` in `Path`
```zsh
openssl rand -hex 32 # AUDIT_HASH_KEY
```

From this directory on *WSL or Linux, with the Podman containers from above up*:
```zsh
cargo run -p discord-bot --example sso_keygen
```

This will output
```
secret_hex (SOPS-encrypt as sso_signing_key): <private key> # BOT_SSO_SIGNING_KEY
public_hex (give to workspace-sync):          <public key> # This is for the admin server, note it for later
```

#### 4. Environment variables

Set up your `.env` in this Directory like so:

```
DISCORD_BOT_TOKEN=`<bot token you were given or made yourself on the dev portal>`
DISCORD_GUILD_ID=`<right click on server -> copy server ID>

SOLIDARITY_TECH_MOCK=1
SOLIDARITY_TECH_BASE_URL=http://127.0.0.1:8000 # or whatever you want

SOLIDARITY_TECH_MOCK_PERSONAS=<your discord user ID from step 3>=good_standing # (e.g.) 12345=good_standing

DATABASE_URL=postgres://postgres@localhost:55432/botonio_dev
DATABASE_DSN=postgres://postgres@localhost:55432/botonio_dev

AUDIT_HASH_KEY=<From step 3.1>

BOT_SSO_ENABLED=1
BOT_SSO_OAUTH_CLIENT_SECRET=<From step 2>
BOT_SSO_OAUTH_CLIENT_ID=<From step 2>
BOT_SSO_SIGNING_KEY=<From step 3.1>
BOT_SSO_CALLER_BEARER=testbearer132 # In prod you should generate this, but it doesn't matter here
BOT_SSO_SOCKET_PATH=/tmp/botonio-sso.sock
BOT_SSO_REDIRECT_URI=<From step 2>
```

#### 5. Discord Setup

Now you can actually run the bot! Hang in there!

From *wsl or Linux*,  run, from this directory:

```zsh
cargo run --bin botonio-botsci
```

Now, hopefully, your bot should light up after this starts in your test server.

**Note**: The following is *not* a *one-time setup*, every time you recreate the `postgres` container with `podman compose` you'll need to rerun this set because the DB clears! Sorry!

Run `/setup` as your `Discord Admin` user (you do *not* need to press any buttons not listed here!):
Click `Verification` -> 
- Unverified role: as you set up in step 2
- Unverified channel: as you set up in step 2
- Manual verification: `mod-verification` from step 2
- Verification log channel: `mod-verification` from step 2
  
Click `back` then `Membership & access` ->
- Member role: `Member` from step 2
- Dues Expired role: `Dues Expired` from step 2
- Dues Expiring role: `Dues Expiring` from step 2
- Manual Verification role: `Manually Verified` from step 2

Click `back` then `Moderators` ->
- In the dropdown select `Discord Admin` from step 2
- toggle `Automatic Membership Checks`: `On`, `SSO: On`

Now *shut the bot down* (this is a bit of jank, SSO only enables for safety if roles for SSO auth are set on boot), `ctrl+c` in your terminal should work.

Once again, run:
```zsh
cargo run --bin botonio-botsci
```

If you did everything right, you should see `INFO botonio_botsci::sso::server: sso: listening on unix socket path=/tmp/botonio-sso.sock`

##### 5.1 Verifying the SSO is working manually

This is optional, you can try and just yolo connect `Rosadmin` if you want.

For one, make sure the `Member` role was added to you in Discord! If not, you messed up `SOLIDARITY_TECH_MOCK_PERSONAS`.

**Optional** running a simple webserver can make it easier to intercept and copy the redirect code, otherwise you need to grab it from the url:
```zsh
python -m http.server 9999 # or whatever port was in your BOT_SSO_REDIRECT_URI
```

Then, run the following
```zsh
SOCK=/tmp/botonio-sso.sock # or whatever you put above in your .env
BEARER=testbearer123 # or whatever you put above in your .env

curl --unix-socket "$SOCK" -X POST -H "Authorization: Bearer $BEARER" http://localhost/sso/begin
```

This will return a a json blob with an `authorize_url` property. Paste that into your browser, and hit "Authorize"

If running the http server, you'll get a `404`, otherwise you'll get a timeout: that's expected, don't panic. Now either from your Browser URL bar, or the python `http.server` you ran. There will be something like `/callback?code=<string>&state=<string>` Run the below *quickly*

(Important, do *not* use single quotes instead of escaping in the CURL string, you'll either break the json parsing or fail to properly substitute the environment variables)
```zsh
CODE=<code param>
STATE=<state param>

curl --unix-socket "$SOCK" -X POST -H "Authorization: Bearer $BEARER" -H "Content-Type: application/json" -d "{\"code\": \"$CODE\", \"state\": \"$STATE\"}" http://localhost/sso/complete
```

With any luck, this gets you a response like: `{"assertion":"<authorized string>"}`. Congrats, you did everything right! Good job!

#### 6. Rosadmin one-off check

> **Important**: This is here for posterity, but the authoritative docs live at https://github.com/portland-dsa/rosadmin - these may be out of sync!

This is just for a *smoke check* that we're up and running like the `curl` test. However, it only verifies `begin`, not the whole flow. See the actual `rosadmin` documentation for running the whole server flow.

Set up a .env file (or the vars directly in your terminal) like so:

```
BOTONIO_SSO_PUBKEY=<From 3.1>
BOTONIO_SSO_BEARER=testbearer123 # or whatever you picked
BOTONIO_SSO_SOCKET_PATH=/tmp/botonio-sso.sock # or whatever you picked
BOTONIO_SSO_GUILD_ID=<From step 3>
BOTONIO_REDIRECT_URI=<From step 2>
BOTONIO_SSO_AUD=rosadmin
BOTONIO_SSO_ISS=botonio
BOTONIO_SSO_KID=v1
```

```zsh
uv run --env-file .env rosadmin one-shot sso-reachability
```

This will return an auth URL like above if successful that you can validate with `curl` as in step `5.1`.