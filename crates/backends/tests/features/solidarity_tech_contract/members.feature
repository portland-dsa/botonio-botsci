@solidarity_tech @contract
Feature: Espio reads and writes members through the Solidarity Tech API

  Espio is the Solidarity Tech client. These scenarios pin the wire contract
  against a mocked API: finding members by email or phone, paging results,
  mapping the org-level Discord custom properties, and merging an identity write
  without clobbering the rest of the record - all carrying the bearer token.

  Background:
    Given a mocked Solidarity Tech API

  @auth
  Scenario: Every request carries the bearer token
    When Espio makes any Solidarity Tech request
    Then the request carries the bearer authorization header

  @lookup
  Scenario: Finding members by email returns every match
    Given Solidarity Tech has one user with the email "espio@example.com"
    When Espio finds members by the email "espio@example.com"
    Then that user is returned

  @lookup
  Scenario: Finding members by phone returns every match
    Given Solidarity Tech has one user with the phone "+15035550123"
    When Espio finds members by the phone "+15035550123"
    Then that user is returned

  @lookup
  Scenario: A lookup with both an email and a phone sends both on one request
    Given Solidarity Tech has a user matching the email "both@example.com" and the phone "+15035551234"
    When Espio finds members by the email "both@example.com" and the phone "+15035551234"
    Then that user is returned

  @lookup
  Scenario: A lookup needs at least an email or a phone
    When Espio finds members with neither an email nor a phone
    Then the lookup fails because no identifier was given

  @properties
  Scenario: The Discord custom properties are mapped onto the member
    Given Solidarity Tech has a user with a discord-handle and discord-user-id property
    When Espio finds that member
    Then the member carries the Discord handle and Discord user id from those properties

  @properties @dues
  Scenario: The dues-status custom properties are decoded onto the member
    Given Solidarity Tech has a user with monthly and yearly dues-status properties
    When Espio finds that dues member
    Then the member carries the monthly and yearly dues status from those properties

  @properties @dues
  Scenario: An unrecognized dues-status value fails the lookup
    Given Solidarity Tech has a user with an unrecognized dues-status value
    When Espio finds that dues member
    Then the lookup fails because the dues status was unrecognized

  @properties @verification
  Scenario: The expiry, membership type, and membership status are decoded onto the member
    Given Solidarity Tech has a user with x-date, membership-type, and membership-status properties
    When Espio finds that dues member
    Then the member carries the expiry date, membership type, and membership standing from those properties

  @properties @verification
  Scenario: An unrecognized membership-type value fails the lookup
    Given Solidarity Tech has a user with an unrecognized membership-type value
    When Espio finds that dues member
    Then the lookup fails because the membership type was unrecognized

  @properties @verification
  Scenario: A retired membership-status value fails the lookup
    Given Solidarity Tech has a user with a retired membership-status value
    When Espio finds that dues member
    Then the lookup fails because the membership status could not be decoded

  @lookup
  Scenario: A filtered lookup returns every match on the single bounded page
    Given Solidarity Tech returns both matches on one bounded page
    When Espio finds the members
    Then both matches are returned

  @lookup @truncation
  Scenario: A lookup the API reports as truncated returns only the page it read
    Given Solidarity Tech returns one match but reports that more exist
    When Espio finds the members
    Then only the page it read is returned and no further page is fetched

  @pagination
  Scenario: Listing all members pages through the whole collection by offset
    Given Solidarity Tech has 150 users across two pages
    When Espio lists all members
    Then all 150 members are returned across both pages

  @write
  Scenario: Stamping a Discord identity merges into the existing properties
    When Espio stamps a Discord handle and id onto a member
    Then the Solidarity Tech update sets the discord-handle and discord-user-id properties

  @write
  Scenario: Stamping only a Discord handle merges just that property
    When Espio stamps only a Discord handle onto a member
    Then the Solidarity Tech update sets only the discord-handle property

  @write
  Scenario: Stamping an alternate email merges just that property
    When Espio stamps an alternate email onto a member
    Then the Solidarity Tech update sets only the alternate-email property

  @write @dry_run
  Scenario: A dry-run handle write makes no request
    When Espio stamps a Discord handle as a dry run
    Then no Solidarity Tech request is made

  @write
  Scenario: Clearing a Discord identity blanks the chosen properties
    When Espio clears a member's Discord handle and id
    Then the Solidarity Tech update blanks the discord-handle and discord-user-id properties

  @write @clear
  Scenario: Clearing only the Discord handle leaves the user-id property untouched
    When Espio clears only a member's Discord handle
    Then only the discord-handle property is blanked

  @write @clear
  Scenario: Clearing only the Discord user id leaves the handle property untouched
    When Espio clears only a member's Discord user id
    Then only the discord-user-id property is blanked

  @write @clear
  Scenario: Clearing with neither property selected makes no request
    When Espio clears neither Discord property
    Then no Solidarity Tech request is made

  @write @clear @dry_run
  Scenario: Clearing a Discord identity as a dry run makes no request
    When Espio clears a member's Discord identity as a dry run
    Then no Solidarity Tech request is made

  @write @dry_run
  Scenario: A dry-run write makes no request
    When Espio stamps a Discord identity as a dry run
    Then no Solidarity Tech request is made

  @properties
  Scenario: The custom property catalog can be listed
    Given Solidarity Tech defines several custom user properties
    When Espio lists the custom user properties
    Then the defined properties are returned
