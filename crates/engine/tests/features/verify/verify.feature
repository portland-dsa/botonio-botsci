Feature: Moderator verifies a member

  Scenario: Verifying a member known only by handle backfills their id
    Given Tails is in our records by handle with no Discord id
    When Sonic verifies Tails
    Then Tails is assigned the Member role
    And Tails's Discord identity is written back to our records
    And the verification is recorded in the audit log

  Scenario: Verifying an already-linked member repairs a drifted handle
    Given Knuckles is in our records linked to his Discord id, under an old handle
    When Sonic verifies Knuckles
    Then Knuckles is assigned the Member role
    And Knuckles's handle is written back to our records
    And the verification is recorded in the audit log

  Scenario: Verifying strips every other managed role the member already holds
    Given Knuckles is in our records linked to his Discord id, under an old handle
    And Knuckles also holds the Unverified role
    And Knuckles also holds the DuesExpired role
    When Sonic verifies Knuckles
    Then Knuckles is assigned the Member role
    And the Unverified and DuesExpired roles are stripped from Knuckles

  Scenario: Verifying someone we do not know assigns Unverified
    Given Shadow is not in our records
    When Sonic verifies Shadow
    Then Shadow is assigned the Unverified role
    And nothing is written back to our records
    And the verification is recorded in the audit log

  Scenario: A handle owned by a different account is refused
    Given Eggman's handle is on record for a different account
    When Sonic verifies Eggman
    Then the verification is refused
    And nothing is written back to our records
    And the conflict is recorded in the audit log

  Scenario: A role is never granted without an audit row
    Given Tails is in our records by handle with no Discord id
    And the audit log is unavailable
    When Sonic verifies Tails
    Then Tails is not assigned any role
    And nothing is written back to our records

  Scenario: A failed role write records a reconciling audit row
    Given Tails is in our records by handle with no Discord id
    And assigning roles is failing
    When Sonic verifies Tails
    Then the verification fails with an error
    And the audit log records the attempt and its failure
