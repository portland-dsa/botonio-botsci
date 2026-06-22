Feature: Stripping managed roles for testing
  A moderator clears the managed roles across the server so bulk verification can be
  retested from a clean slate. Members who were hand-approved are forgotten outright;
  everyone else only loses their Discord roles, with the database left alone.

  Scenario: An overridden member is fully forgotten
    Given Knuckles was hand-approved and holds the Member role and the override marker
    When Sonic strips Knuckles
    Then Knuckles's managed roles are stripped
    And Knuckles's override marker is cleared
    And Knuckles's cache link is cleared
    And Knuckles's override stamp is deleted
    And the reset is recorded in the audit log

  Scenario: A plain member loses roles but keeps their cache link
    Given Tails holds the Member role and is known to us
    When Sonic strips Tails
    Then Tails's managed roles are stripped
    And Tails's cache link is intact
    And no reset is recorded in the audit log

  Scenario: A stale override marker is swept even without a stamp
    Given Shadow holds the Unverified role and a stale override marker
    When Sonic strips Shadow
    Then Shadow's managed roles are stripped
    And Shadow's override marker is cleared
    And no reset is recorded in the audit log
