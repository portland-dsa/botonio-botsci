Feature: Throttling on-demand cache refreshes
  /refresh-cache re-warms the member cache from Solidarity Tech, but a process-wide
  cooldown stops a moderator using it to hammer the API.

  Scenario: The first refresh is allowed
    When Sonic refreshes the cache
    Then the refresh is allowed

  Scenario: A second refresh inside the window is refused
    When Sonic refreshes the cache
    And Sonic refreshes the cache again 60 seconds later
    Then the refresh is refused with 120 seconds left

  Scenario: A refresh after the window is allowed again
    When Sonic refreshes the cache
    And Sonic refreshes the cache again 181 seconds later
    Then the refresh is allowed
