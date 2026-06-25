Feature: Dues-reminder milestone selection

  Scenario Outline: the milestone a member is due
    Given Sonic's membership lapses in <days> days
    And he was last sent the <last_sent> reminder
    And the sweep is <timeliness>
    Then the reminder due is <milestone>

    Examples:
      | days | last_sent | timeliness | milestone |
      | 30   | none      | timely     | Days30    |
      | 14   | Days30    | timely     | Days14    |
      | 1    | Days14    | timely     | Day1      |
      | 20   | Days30    | timely     | none      |
      | 25   | none      | delayed    | Days30    |
      | 22   | none      | delayed    | Days14    |
      | 21   | none      | delayed    | Days14    |
      | 5    | none      | delayed    | Day1      |
      | 31   | none      | delayed    | none      |
      | 100  | none      | delayed    | none      |
      | -1   | none      | timely     | Expired   |
      | -3   | Expired   | timely     | none      |
