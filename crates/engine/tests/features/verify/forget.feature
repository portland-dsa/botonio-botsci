Feature: Forgetting a member for testing
  A moderator resets a member's bot state so the verify and override paths can be retested.

  Scenario: Forget strips roles, clears the cache link, and deletes the stamp
    Given Knuckles was hand-approved by override
    When Sonic forgets Knuckles
    Then Knuckles's managed roles are stripped
    And Knuckles's cache link is cleared
    And Knuckles's override stamp is deleted
    And the reset is recorded in the audit log
