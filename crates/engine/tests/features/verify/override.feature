Feature: Moderator hand-approves a member past Solidarity Tech

  Scenario: Sonic overrides Silver when no record can be matched
    Given Silver is not in our records
    When Sonic overrides Silver
    Then Silver is assigned the Member role
    And the override marker is assigned to Silver
    And the approval stamp is recorded
    And the override is recorded in the audit log with method override
