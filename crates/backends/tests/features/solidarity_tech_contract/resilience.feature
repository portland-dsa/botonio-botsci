@solidarity_tech @contract @resilience
Feature: Espio stays within Solidarity Tech's rate limit and surfaces errors

  Espio is the Solidarity Tech client. These scenarios pin how the client
  behaves under rate limiting and error responses: it backs off and retries a
  429, paces sequential requests, and surfaces 4xx/5xx as a Solidarity Tech
  error rather than swallowing them.

  Background:
    Given a mocked Solidarity Tech API

  @retry
  Scenario: A rate-limited request is retried after backing off
    Given Solidarity Tech returns 429 once and then succeeds
    When Espio makes a Solidarity Tech request
    Then the Solidarity Tech request is retried and ultimately succeeds

  @pacing
  Scenario: Sequential requests are paced under the rate limit
    When Espio makes five sequential Solidarity Tech requests
    Then the Solidarity Tech requests are spaced out under the rate limit

  @error
  Scenario: A server error surfaces as a Solidarity Tech error
    Given Solidarity Tech responds with status 500
    When Espio makes a Solidarity Tech request
    Then the request fails with a Solidarity Tech error

  @error
  Scenario: An unauthorized response surfaces as a Solidarity Tech error
    Given Solidarity Tech responds with status 401
    When Espio makes a Solidarity Tech request
    Then the request fails with a Solidarity Tech error
