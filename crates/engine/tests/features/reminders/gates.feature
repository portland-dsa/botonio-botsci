Feature: Dues-reminder gates

  Scenario: an opted-out member still gets the expiry notice
    Given Tails lapsed yesterday
    And Tails has opted out of dues reminders
    When the reminder sweep is planned
    Then Tails is due the Expired reminder

  Scenario: a grace holds back even the expiry notice
    Given Tails lapsed yesterday
    And Tails has an active grace
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: an auto-renewing member is left alone
    Given Amy's membership lapses in 14 days
    And Amy's monthly dues are active
    When the reminder sweep is planned
    Then Amy is due no reminder

  Scenario: a snoozed member skips the pre-lapse nudge
    Given Amy's membership lapses in 14 days
    And Amy is snoozed for this cycle
    When the reminder sweep is planned
    Then Amy is due no reminder

  Scenario: a member with no expiry date is skipped
    Given Tails has no expiry date
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: an unlinked member is skipped
    Given Tails is unlinked
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: a stale cycle resets and fires the fresh milestone
    Given Amy's membership lapses in 14 days
    And Amy's last cycle was for a different xdate
    When the reminder sweep is planned
    Then Amy is due the Days14 reminder
