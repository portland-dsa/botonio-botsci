Feature: Looking up a manually-verified member
  A member Solidarity Tech does not know, whom a moderator hand-approved, has an
  override card rather than a not-found reply.

  Scenario: A moderator looks up a hand-approved member
    Given Sonic is a moderator
    And Knuckles is a manually-verified member approved by Sonic
    When Sonic looks up Knuckles
    Then the override card is shown
    And one audit row records the outcome "override"

  Scenario: A member views their own override card
    Given Knuckles is a manually-verified member approved by Sonic
    When Knuckles looks up Knuckles
    Then the override card is shown
    And no audit row is written
