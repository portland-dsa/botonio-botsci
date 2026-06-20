Feature: Manual verification by email
  When the automatic match misses, a moderator supplies a member's email by hand.

  Scenario: An email finds a member known only by handle and links them
    Given Tails is in our records by handle with no Discord id
    When Sonic verifies Tails by email
    Then Tails is assigned the Member role
    And Tails's Discord identity is written back to our records
    And the email verification is recorded with method email

  Scenario: An email that matches nobody changes nothing
    Given Shadow is not in our records
    When Sonic verifies Shadow by email
    Then the email lookup finds no record
    And nothing is written back to our records
    And the not-found lookup is recorded in the audit log

  Scenario: An email on record for a different account is refused
    Given Eggman's handle is on record for a different account
    When Sonic verifies Eggman by email
    Then the verification is refused
    And nothing is written back to our records
    And the conflict is recorded in the audit log
