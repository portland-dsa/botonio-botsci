Feature: Moderator lookup card

  Scenario: A moderator looks up a member who has a record
    Given Sonic is a moderator
    And Tails is a member with a record
    When Sonic looks up Tails
    Then the card is shown
    And one audit row records the outcome "found"

  Scenario: A moderator looks up someone with no record
    Given Sonic is a moderator
    And Shadow is a member with no record
    When Sonic looks up Shadow
    Then a not-found reply is shown
    And one audit row records the outcome "not_found"

  Scenario: A non-moderator is refused and not audited
    Given Eggman is not a moderator
    And Tails is a member with a record
    When Eggman looks up Tails
    Then the lookup is refused for lack of permission
    And no audit row is written

  Scenario: Anyone may view their own card un-audited
    Given Eggman is not a moderator
    And Eggman is a member with a record
    When Eggman looks up Eggman
    Then the card is shown
    And no audit row is written

  Scenario: A moderator is rate-limited after their allowance
    Given Sonic is a moderator
    And Tails is a member with a record
    When Sonic looks up Tails 11 times
    Then the eleventh lookup is rate-limited
    And ten audit rows are written
