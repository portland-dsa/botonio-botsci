Feature: The staging bot reads fabricated members from the mock

  Scenario: Reading the member list returns the fabricated roster
    Given the mock serves Sonic in good standing and Tails as lapsed
    When Botonio reads the member list
    Then Botonio sees two members
    And Sonic is in good standing
    And Tails has lapsed
