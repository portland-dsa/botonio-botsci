Feature: Looking up a manually-verified member
  A member Solidarity Tech does not know, whom a moderator hand-approved, has an
  override card rather than a not-found reply.

  Scenario: A moderator looks up a hand-approved member with a reason
    Given Sonic is a moderator
    And Knuckles is a manually-verified member approved by Sonic with the reason "paid cash at orientation"
    When Sonic looks up Knuckles
    Then the override card is shown
    And the override card shows the reason "paid cash at orientation"
    And one audit row records the outcome "override"

  Scenario: A member viewing their own override card does not see the reason
    Given Knuckles is a manually-verified member approved by Sonic with the reason "paid cash at orientation"
    When Knuckles looks up Knuckles
    Then the override card is shown
    And the override card hides the reason
    And no audit row is written
