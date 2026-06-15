@discord @live
Feature: Vector drives the real Discord guild through the bot

  These scenarios hit a real test guild through the bot token and run only behind
  the live-discord feature, with credentials supplied out of band. Vector reads a
  member's roles and round-trips role writes against a designated test user,
  leaving them as they were found.

  @read
  Scenario: Vector reads a member's roles
    Given the live Discord credentials and a test user
    When Vector reads the test user's roles
    Then the test user's roles are returned

  @write @role
  Scenario: Vector round-trips a role change on the test user
    Given the live Discord credentials and a test user
    When Vector sets and then restores the test user's status role
    Then the role write succeeds and the test user ends as they began

  @write @role @noop
  Scenario: Setting a role the member already holds is a no-op
    Given the live Discord credentials and a test user already at a known status
    When Vector sets that same status role again
    Then no role change is made
