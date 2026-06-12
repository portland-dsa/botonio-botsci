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