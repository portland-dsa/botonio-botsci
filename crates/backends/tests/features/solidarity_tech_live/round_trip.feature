@solidarity_tech @live
Feature: Espio round-trips against the real Solidarity Tech API

  These scenarios hit the live Solidarity Tech API and run only behind the
  live-solidarity-tech feature, with credentials supplied out of band. The write
  is a no-op: it sets a member's existing Discord identity back onto them so the
  record is unchanged.

  @read
  Scenario: Espio finds a known member by email
    Given the live Solidarity Tech credentials and a known member email
    When Espio finds the known member by email
    Then the known member is returned

  @write @noop
  Scenario: Espio writes a member's existing Discord identity back unchanged
    Given the live Solidarity Tech credentials and a member who already has a Discord identity
    And no-op writes are allowed for this run
    When Espio writes that same Discord identity back
    Then the write succeeds and the record is unchanged
