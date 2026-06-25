Feature: Dues notice selection

  Scenario Outline: <desc>
    Given Sonic's dues expire in <days> days
    And his last sent notice is <last_sent>
    When the reminder planner runs
    Then he is due the <notice> notice

    Examples:
      | desc                       | days | last_sent | notice  |
      | in window, nothing sent    | 10   | none      | renewal |
      | expiry day counts in       | 0    | none      | renewal |
      | renewal already sent       | 10   | renewal   | none    |
      | outside window             | 20   | none      | none    |
      | lapsed                     | -3   | none      | lapse   |
      | offline through window     | -3   | none      | lapse   |
      | lapse already sent         | -3   | lapse     | none    |
