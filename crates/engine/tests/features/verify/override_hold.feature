Feature: An active manual override holds a member at Member during reconciliation

  Scenario: Shadow is unknown to Solidarity Tech but hand-approved - he verifies as Member
    Given Shadow is not in our records
    And Shadow has an active manual override
    When Sonic verifies Shadow
    Then Shadow is assigned the Member role
    And the verification is recorded in the audit log

  Scenario: Rouge's dues lapsed but she is hand-approved - she verifies as Member
    Given Rouge is in our records linked to her Discord id, and her dues have lapsed
    And Rouge has an active manual override
    When Sonic verifies Rouge
    Then Rouge is assigned the Member role

  Scenario: A resync re-grants Member to a hand-approved member who was demoted
    Given Shadow is not in our records
    And Shadow also holds the Unverified role
    And Shadow has an active manual override
    When Sonic resyncs Shadow
    Then Shadow is resynced to Member
    And the resync is audited once
