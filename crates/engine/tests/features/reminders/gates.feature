Feature: Dues-reminder gates

  Scenario: an opted-out member still gets the lapse notice
    Given Tails lapsed yesterday
    And Tails has opted out of dues reminders
    When the reminder sweep is planned
    Then Tails is due the lapse notice

  Scenario: a grace holds back even the lapse notice
    Given Tails lapsed yesterday
    And Tails has an active grace
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: an auto-renewing member skips the renewal notice
    Given Amy's dues expire in 10 days
    And Amy's monthly dues are active
    When the reminder sweep is planned
    Then Amy is due no reminder

  Scenario: opt-out does not suppress lapse for an auto-renewing member who lapsed
    Given Tails lapsed yesterday
    And Tails has opted out of dues reminders
    When the reminder sweep is planned
    Then Tails is due the lapse notice

  Scenario: a member with no expiry date is skipped
    Given Tails has no expiry date
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: an unlinked member is skipped
    Given Tails is unlinked
    When the reminder sweep is planned
    Then Tails is due no reminder

  Scenario: a stale cycle resets and fires the fresh renewal
    Given Amy's dues expire in 10 days
    And Amy's last cycle was for a different xdate
    When the reminder sweep is planned
    Then Amy is due the renewal notice
