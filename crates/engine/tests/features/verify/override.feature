Feature: Moderator hand-approves a member past Solidarity Tech

  Scenario: Sonic overrides Silver when no record can be matched
    Given Silver is not in our records
    When Sonic overrides Silver
    Then Silver is assigned the Member role
    And the override marker is assigned to Silver
    And the approval stamp is recorded
    And the override is recorded in the audit log with method override

  Scenario: Sonic overrides Silver with a reason
    Given Silver is not in our records
    When Sonic overrides Silver with the reason "vouched at the branch meeting"
    Then Silver is assigned the Member role
    And the approval stamp records the reason "vouched at the branch meeting"

  Scenario: Override survives a failed marker write
    Given Silver is not in our records
    And the override marker write is failing
    When Sonic overrides Silver
    Then Silver is assigned the Member role
    And the override marker is not assigned to Silver
    And the marker failure is recorded in the audit log
    And the approval stamp is recorded
